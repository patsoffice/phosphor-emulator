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

use crate::core::bus::BusMaster;
use crate::core::watchpoint::{DebugAccessSource, WatchpointPhase, Watchpoints};

// Canonical watchpoint types live in `core::watchpoint`; re-exported here so
// existing `memory_map::WatchpointHit` paths keep working during migration.
pub use crate::core::watchpoint::{WatchpointHit, WatchpointKind};

/// Machine-defined region identifier. Values are assigned by each machine
/// as constants (e.g., `const VIDEO_RAM: RegionId = 1`). The MemoryMap
/// stores and reports them but does not interpret them.
pub type RegionId = u8;

/// Sentinel region ID for unmapped pages.
pub const UNMAPPED: RegionId = 0;

/// What kind of access a region supports (for introspection and display).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AccessKind {
    ReadWrite,
    ReadOnly,
    WriteOnly,
    Io,
    Unmapped,
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

/// Result of a side-effect-free debug read from an address space.
///
/// Canonical memory-result type shared with debug-observability (see
/// `docs/designs/address-space-refactor.md`). Lets the debugger label
/// memory cells as backed, I/O, or unmapped instead of displaying `FF`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DebugRead {
    /// The address resolves to backing storage.
    Backed {
        /// Value read (low `width * 8` bits significant).
        value: u32,
        /// Access width in bytes (1, 2, or 4).
        width: u8,
        /// Region the address resolved to.
        region_id: RegionId,
    },
    /// The address is mapped to I/O; reading it would have side effects.
    Io,
    /// The address is not mapped.
    Unmapped,
}

/// Result of a side-effect-free debug write to an address space.
///
/// Canonical memory-result type shared with debug-observability. Debug
/// writes may modify ROM backing; `access` lets the UI distinguish
/// "patched Program ROM" from "edited RAM".
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DebugWrite {
    /// The write landed in backing storage.
    Backed {
        /// Value before the write.
        old: u32,
        /// Value after the write.
        new: u32,
        /// Access width in bytes (1, 2, or 4).
        width: u8,
        /// Region the address resolved to.
        region_id: RegionId,
        /// Access kind of the region (e.g. `ReadOnly` indicates a ROM patch).
        access: AccessKind,
    },
    /// The address is mapped to I/O; the write was ignored.
    IoIgnored,
    /// The address is not mapped; the write was ignored.
    UnmappedIgnored,
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

/// Named byte storage for RAM/ROM/NVRAM/inactive banks, indexed by region.
///
/// Owns a flat `Vec<u8>` plus per-region offset/length tables. It knows
/// nothing about address decode; callers resolve an address to a
/// `(region_id, region-local offset)` pair first (via [`AddressMap16`]) and
/// read or write through that. Shared by the 16-bit and (future) 32-bit
/// address spaces.
pub struct MemoryBacking {
    /// Flat backing store for all regions with storage.
    data: Vec<u8>,
    /// Offset into `data` for each region_id. `u32::MAX` = no backing (I/O).
    region_backing: [u32; 256],
    /// Byte length of each region's backing.
    region_lengths: [u32; 256],
}

impl MemoryBacking {
    /// Create empty storage with no regions allocated.
    pub fn new() -> Self {
        Self {
            data: Vec::new(),
            region_backing: [u32::MAX; 256],
            region_lengths: [0; 256],
        }
    }

    /// Allocate `length` zeroed bytes of backing for `region_id`.
    pub fn allocate(&mut self, region_id: impl Into<RegionId>, length: u32) {
        let region_id = region_id.into();
        let offset = self.data.len() as u32;
        self.data.resize(self.data.len() + length as usize, 0);
        self.region_backing[region_id as usize] = offset;
        self.region_lengths[region_id as usize] = length;
    }

    /// True if `region_id` has backing storage allocated.
    pub fn has_region(&self, region_id: impl Into<RegionId>) -> bool {
        self.region_backing[region_id.into() as usize] != u32::MAX
    }

    /// Get a read-only slice of a region's backing store.
    ///
    /// Panics if the region has no backing (I/O or unregistered).
    pub fn region_data(&self, region_id: impl Into<RegionId>) -> &[u8] {
        let region_id = region_id.into();
        let offset = self.region_backing[region_id as usize];
        debug_assert!(
            offset != u32::MAX,
            "region_data called on region {region_id} with no backing"
        );
        let offset = offset as usize;
        let length = self.region_lengths[region_id as usize] as usize;
        &self.data[offset..offset + length]
    }

    /// Get a mutable slice of a region's backing store.
    ///
    /// Panics if the region has no backing (I/O or unregistered).
    pub fn region_data_mut(&mut self, region_id: impl Into<RegionId>) -> &mut [u8] {
        let region_id = region_id.into();
        let offset = self.region_backing[region_id as usize];
        debug_assert!(
            offset != u32::MAX,
            "region_data_mut called on region {region_id} with no backing"
        );
        let offset = offset as usize;
        let length = self.region_lengths[region_id as usize] as usize;
        &mut self.data[offset..offset + length]
    }

    /// Bulk-copy data into a region's backing store (e.g., ROM loading).
    ///
    /// `data` must exactly match the region's length.
    pub fn load_region(&mut self, region_id: impl Into<RegionId>, data: &[u8]) {
        let region_id = region_id.into();
        let dest = self.region_data_mut(region_id);
        assert_eq!(
            dest.len(),
            data.len(),
            "load_region: data length {} doesn't match region {} length {}",
            data.len(),
            region_id,
            dest.len()
        );
        dest.copy_from_slice(data);
    }

    /// Copy data into a region's backing store at the given byte offset.
    pub fn load_region_at(&mut self, region_id: impl Into<RegionId>, offset: usize, data: &[u8]) {
        let region_id = region_id.into();
        let dest = self.region_data_mut(region_id);
        let end = (offset + data.len()).min(dest.len());
        let len = end - offset;
        dest[offset..end].copy_from_slice(&data[..len]);
    }

    /// Side-effect-free read at a region-local offset. Returns `None` if
    /// the region has no backing (I/O or unregistered).
    #[inline]
    pub fn read_region_offset(&self, region_id: RegionId, offset: usize) -> Option<u8> {
        let backing_offset = self.region_backing[region_id as usize];
        if backing_offset == u32::MAX {
            return None;
        }
        Some(self.data[backing_offset as usize + offset])
    }

    /// Side-effect-free write at a region-local offset. Returns false (and
    /// ignores the write) if the region has no backing.
    #[inline]
    pub fn write_region_offset(&mut self, region_id: RegionId, offset: usize, data: u8) -> bool {
        let backing_offset = self.region_backing[region_id as usize];
        if backing_offset == u32::MAX {
            return false;
        }
        self.data[backing_offset as usize + offset] = data;
        true
    }

    /// Read at a region-local offset (hot-path version).
    ///
    /// Only call on regions with backing (RAM/ROM). Panics in debug builds
    /// if the region has no backing.
    #[inline(always)]
    pub fn read(&self, region_id: RegionId, offset: usize) -> u8 {
        let backing_offset = self.region_backing[region_id as usize];
        debug_assert!(
            backing_offset != u32::MAX,
            "backing read on region {region_id} with no backing (offset={offset:#06X})"
        );
        self.data[backing_offset as usize + offset]
    }

    /// Write at a region-local offset (hot-path version).
    ///
    /// Only call on regions with backing (RAM/ROM). Panics in debug builds
    /// if the region has no backing.
    #[inline(always)]
    pub fn write(&mut self, region_id: RegionId, offset: usize, data: u8) {
        let backing_offset = self.region_backing[region_id as usize];
        debug_assert!(
            backing_offset != u32::MAX,
            "backing write on region {region_id} with no backing (offset={offset:#06X})"
        );
        self.data[backing_offset as usize + offset] = data;
    }
}

impl Default for MemoryBacking {
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
}

/// Migration-only alias for [`AddressSpace16`].
///
/// New code should use `AddressSpace16` directly; this alias exists so
/// current machines keep compiling while call sites migrate, and will be
/// removed once migration completes (see
/// `docs/designs/address-space-refactor.md`, Phase 7).
pub type MemoryMap = AddressSpace16;

impl AddressSpace16 {
    /// Create a new address space with all pages unmapped.
    pub fn new() -> Self {
        Self {
            map: AddressMap16::new(),
            backing: MemoryBacking::new(),
            active_watch_count: 0,
            watchpoints: Watchpoints::new(),
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
    #[inline]
    pub fn debug_read(&self, addr: u16) -> Option<u8> {
        let page = self.page(addr);
        self.backing
            .read_region_offset(page.region_id, self.map.region_offset(addr))
    }

    /// Side-effect-free write to backing memory. No-op for I/O and unmapped regions.
    #[inline]
    pub fn debug_write(&mut self, addr: u16, data: u8) {
        let page = self.map.page(addr);
        self.backing
            .write_region_offset(page.region_id, self.map.region_offset(addr), data);
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

    /// Check for a read watchpoint hit. Returns true if the exact address
    /// is read-watched, queueing a hit.
    ///
    /// When no watchpoints are set anywhere (`active_watch_count == 0`),
    /// this compiles to a single branch-not-taken — effectively zero cost.
    #[inline(always)]
    pub fn check_read_watch(&mut self, addr: u16, value: u8) -> bool {
        if self.active_watch_count == 0 {
            return false;
        }
        let page = &self.map.pages[(addr >> 8) as usize];
        if page.watch_read
            && self
                .watchpoints
                .matches(0, addr as u32, WatchpointKind::Read)
        {
            let hit = self.make_hit(addr, WatchpointKind::Read, value);
            self.watchpoints.push_hit(hit);
            return true;
        }
        false
    }

    /// Check for a write watchpoint hit. Returns true if the exact address
    /// is write-watched, queueing a hit.
    #[inline(always)]
    pub fn check_write_watch(&mut self, addr: u16, value: u8) -> bool {
        if self.active_watch_count == 0 {
            return false;
        }
        let page = &self.map.pages[(addr >> 8) as usize];
        if page.watch_write
            && self
                .watchpoints
                .matches(0, addr as u32, WatchpointKind::Write)
        {
            let hit = self.make_hit(addr, WatchpointKind::Write, value);
            self.watchpoints.push_hit(hit);
            return true;
        }
        false
    }

    /// Build a canonical hit for a byte access through this map.
    ///
    /// The observability metadata (`source`, `cycle`, `pc`) stays at its
    /// defaults here; populating it at the bus/board boundary is
    /// debug-observability work.
    fn make_hit(&self, addr: u16, kind: WatchpointKind, value: u8) -> WatchpointHit {
        WatchpointHit {
            cpu_index: 0,
            source: DebugAccessSource::Unknown,
            cycle: 0,
            pc: None,
            addr: addr as u32,
            kind,
            phase: WatchpointPhase::After,
            value: value as u32,
            width: 1,
            region: self.region_at(addr).map(|r| r.name),
            device: None,
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

    /// Set a watchpoint on the exact address `addr`.
    ///
    /// The page-level flag is set as a fast filter; the exact address is
    /// recorded in the watchpoint set so only that address fires.
    pub fn set_watchpoint(&mut self, addr: u16, kind: WatchpointKind) {
        self.watchpoints.set(0, addr as u32, kind);
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
    pub fn clear_watchpoint(&mut self, addr: u16, kind: WatchpointKind) {
        self.watchpoints.clear(0, addr as u32, kind);

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

/// Storage-only tests for the bare [`MemoryBacking`] (no decode).
#[cfg(test)]
mod memory_backing_tests {
    use super::*;

    const RAM: RegionId = 1;
    const ROM: RegionId = 2;
    const IO: RegionId = 3;

    #[test]
    fn allocate_zeroed_backing() {
        let mut backing = MemoryBacking::new();
        backing.allocate(RAM, 0x0400);

        assert!(backing.has_region(RAM));
        assert!(!backing.has_region(IO));
        assert_eq!(backing.region_data(RAM).len(), 0x0400);
        assert!(backing.region_data(RAM).iter().all(|&b| b == 0));
    }

    #[test]
    fn regions_get_independent_slices() {
        let mut backing = MemoryBacking::new();
        backing.allocate(RAM, 0x100);
        backing.allocate(ROM, 0x100);

        backing.region_data_mut(RAM)[0] = 0x11;
        backing.region_data_mut(ROM)[0] = 0x22;

        assert_eq!(backing.region_data(RAM)[0], 0x11);
        assert_eq!(backing.region_data(ROM)[0], 0x22);
    }

    #[test]
    fn load_whole_region() {
        let mut backing = MemoryBacking::new();
        backing.allocate(ROM, 0x0400);

        let rom_data: Vec<u8> = (0..0x0400).map(|i| (i & 0xFF) as u8).collect();
        backing.load_region(ROM, &rom_data);

        assert_eq!(backing.region_data(ROM)[0x00], 0x00);
        assert_eq!(backing.region_data(ROM)[0xFF], 0xFF);
        assert_eq!(backing.region_data(ROM)[0x100], 0x00);
    }

    #[test]
    fn load_region_at_offset() {
        let mut backing = MemoryBacking::new();
        backing.allocate(ROM, 0x300);

        backing.load_region_at(ROM, 0x100, &[0xAA, 0xBB, 0xCC]);

        assert_eq!(backing.region_data(ROM)[0x100], 0xAA);
        assert_eq!(backing.region_data(ROM)[0x102], 0xCC);
        assert_eq!(backing.region_data(ROM)[0x103], 0x00);
    }

    #[test]
    fn read_write_region_offset() {
        let mut backing = MemoryBacking::new();
        backing.allocate(RAM, 0x100);

        assert!(backing.write_region_offset(RAM, 0x42, 0xBE));
        assert_eq!(backing.read_region_offset(RAM, 0x42), Some(0xBE));
        assert_eq!(backing.read(RAM, 0x42), 0xBE);

        backing.write(RAM, 0x43, 0xEF);
        assert_eq!(backing.read_region_offset(RAM, 0x43), Some(0xEF));
    }

    #[test]
    fn missing_backing_reads_none_writes_ignored() {
        let mut backing = MemoryBacking::new();
        backing.allocate(RAM, 0x100);

        assert_eq!(backing.read_region_offset(IO, 0x00), None);
        assert!(!backing.write_region_offset(IO, 0x00, 0xFF));
        assert!(!backing.has_region(IO));
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
        let map = MemoryMap::new();
        for page_idx in 0..=255u8 {
            let addr = (page_idx as u16) << 8;
            assert_eq!(map.page(addr).region_id, UNMAPPED);
        }
        assert!(map.regions().is_empty());
    }

    #[test]
    fn region_populates_pages_and_descriptor() {
        let mut map = MemoryMap::new();
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
        let mut map = MemoryMap::new();
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
        let mut map = MemoryMap::new();
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
        let mut map = MemoryMap::new();
        map.region(RAM, "RAM", 0x0000, 0x8000, AccessKind::ReadWrite)
            .region(ROM, "ROM", 0x8000, 0x8000, AccessKind::ReadOnly);

        // Pac-Man style: mirror lower 32K to upper 32K
        // (In practice you'd mirror before setting ROM, but this tests the mechanic)
        let mut map2 = MemoryMap::new();
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
        let mut map = MemoryMap::new();
        map.region(RAM, "Work RAM", 0x4C00, 0x0400, AccessKind::ReadWrite)
            .mirror(0xCC00, 0x4C00, 0x0400);

        let src = map.page(0x4C00);
        let dst = map.page(0xCC00);
        assert_eq!(src.region_id, dst.region_id);
        assert_eq!(src.base_offset, dst.base_offset);
    }

    #[test]
    fn remap_pages_for_bank_switching() {
        let mut map = MemoryMap::new();
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
        let map = MemoryMap::new();
        assert!(!map.has_any_watchpoints());
        assert_eq!(map.active_watch_count, 0);
    }

    #[test]
    fn check_watch_is_noop_when_no_watchpoints() {
        let mut map = MemoryMap::new();
        map.region(RAM, "RAM", 0x0000, 0x10000, AccessKind::ReadWrite);

        assert!(!map.check_read_watch(0x1234, 0x42));
        assert!(!map.check_write_watch(0x1234, 0x42));
        assert!(map.take_hit().is_none());
    }

    #[test]
    fn read_watchpoint_fires() {
        let mut map = MemoryMap::new();
        map.region(RAM, "RAM", 0x0000, 0x10000, AccessKind::ReadWrite);

        map.set_watchpoint(0x4000, WatchpointKind::Read);
        assert!(map.has_any_watchpoints());
        assert_eq!(map.active_watch_count, 1);

        // Read at the exact watched address → hit
        assert!(map.check_read_watch(0x4000, 0xAB));
        let hit = map.take_hit().unwrap();
        assert_eq!(hit.addr, 0x4000);
        assert_eq!(hit.kind, WatchpointKind::Read);
        assert_eq!(hit.value, 0xAB);

        // Read at a different address on the same page → no hit (exact match only)
        assert!(!map.check_read_watch(0x4042, 0xCD));

        // Write at the watched address → no hit (only read watchpoint set)
        assert!(!map.check_write_watch(0x4000, 0xCD));
        assert!(map.take_hit().is_none());

        // Read in a different page → no hit
        assert!(!map.check_read_watch(0x5000, 0x00));
    }

    #[test]
    fn write_watchpoint_fires() {
        let mut map = MemoryMap::new();
        map.region(RAM, "RAM", 0x0000, 0x10000, AccessKind::ReadWrite);

        map.set_watchpoint(0x1000, WatchpointKind::Write);

        assert!(map.check_write_watch(0x1000, 0x99));
        let hit = map.take_hit().unwrap();
        assert_eq!(hit.addr, 0x1000);
        assert_eq!(hit.kind, WatchpointKind::Write);
        assert_eq!(hit.value, 0x99);

        // Read on same page → no hit
        assert!(!map.check_read_watch(0x1000, 0x00));
    }

    #[test]
    fn both_read_and_write_watchpoint() {
        let mut map = MemoryMap::new();
        map.region(RAM, "RAM", 0x0000, 0x10000, AccessKind::ReadWrite);

        map.set_watchpoint(0x2000, WatchpointKind::Read);
        map.set_watchpoint(0x2000, WatchpointKind::Write);
        // Same page, so active_watch_count should still be 1
        assert_eq!(map.active_watch_count, 1);

        assert!(map.check_read_watch(0x2000, 0x11));
        map.take_hit();
        assert!(map.check_write_watch(0x2000, 0x22));
    }

    #[test]
    fn clear_watchpoint() {
        let mut map = MemoryMap::new();
        map.region(RAM, "RAM", 0x0000, 0x10000, AccessKind::ReadWrite);

        map.set_watchpoint(0x3000, WatchpointKind::Read);
        map.set_watchpoint(0x3000, WatchpointKind::Write);
        assert_eq!(map.active_watch_count, 1);

        // Clear read but keep write
        map.clear_watchpoint(0x3000, WatchpointKind::Read);
        assert_eq!(map.active_watch_count, 1); // still active (write remains)
        assert!(!map.check_read_watch(0x3000, 0x00));
        assert!(map.check_write_watch(0x3000, 0x00));

        // Clear write too
        map.take_hit();
        map.clear_watchpoint(0x3000, WatchpointKind::Write);
        assert_eq!(map.active_watch_count, 0);
        assert!(!map.has_any_watchpoints());
    }

    #[test]
    fn clear_all_watchpoints() {
        let mut map = MemoryMap::new();
        map.region(RAM, "RAM", 0x0000, 0x10000, AccessKind::ReadWrite);

        map.set_watchpoint(0x1000, WatchpointKind::Read);
        map.set_watchpoint(0x2000, WatchpointKind::Write);
        map.set_watchpoint(0x3000, WatchpointKind::Read);
        assert_eq!(map.active_watch_count, 3);

        map.clear_all_watchpoints();
        assert_eq!(map.active_watch_count, 0);
        assert!(!map.has_any_watchpoints());
        assert!(!map.check_read_watch(0x1000, 0x00));
        assert!(!map.check_write_watch(0x2000, 0x00));
    }

    #[test]
    fn multiple_watchpoints_on_different_pages() {
        let mut map = MemoryMap::new();
        map.region(RAM, "RAM", 0x0000, 0x10000, AccessKind::ReadWrite);

        map.set_watchpoint(0x1000, WatchpointKind::Read);
        map.set_watchpoint(0x2000, WatchpointKind::Read);
        assert_eq!(map.active_watch_count, 2);

        // Only exact addresses fire
        assert!(!map.check_read_watch(0x0000, 0x00));
        assert!(map.check_read_watch(0x1000, 0x42));
        map.take_hit();
        // Different address on same page → no hit
        assert!(!map.check_read_watch(0x1050, 0x00));
    }

    #[test]
    fn watchpoint_exact_address_only() {
        let mut map = MemoryMap::new();
        map.region(RAM, "RAM", 0x0000, 0x10000, AccessKind::ReadWrite);

        map.set_watchpoint(0x2042, WatchpointKind::Write);

        // Same page, different address → no hit
        assert!(!map.check_write_watch(0x2000, 0x11));
        assert!(!map.check_write_watch(0x2041, 0x22));
        assert!(!map.check_write_watch(0x2043, 0x33));
        assert!(!map.check_write_watch(0x20FF, 0x44));

        // Exact address → hit
        assert!(map.check_write_watch(0x2042, 0x55));
        let hit = map.take_hit().unwrap();
        assert_eq!(hit.addr, 0x2042);
        assert_eq!(hit.value, 0x55);
    }

    #[test]
    fn clear_one_of_two_on_same_page() {
        let mut map = MemoryMap::new();
        map.region(RAM, "RAM", 0x0000, 0x10000, AccessKind::ReadWrite);

        map.set_watchpoint(0x3000, WatchpointKind::Read);
        map.set_watchpoint(0x3010, WatchpointKind::Read);
        assert_eq!(map.active_watch_count, 1); // same page

        // Clear the first; page flag should stay (0x3010 still active)
        map.clear_watchpoint(0x3000, WatchpointKind::Read);
        assert_eq!(map.active_watch_count, 1);
        assert!(!map.check_read_watch(0x3000, 0x00)); // cleared
        assert!(map.check_read_watch(0x3010, 0xAA)); // still active
    }

    // -----------------------------------------------------------------------
    // Introspection tests
    // -----------------------------------------------------------------------

    #[test]
    fn region_at_returns_descriptor() {
        let mut map = MemoryMap::new();
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
        let mut map = MemoryMap::new();
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
        let mut map = MemoryMap::new();
        map.region(RAM, "RAM", 0x0000, 0x10000, AccessKind::ReadWrite);
        map.set_watchpoint(0x1000, WatchpointKind::Read);
        map.set_watchpoint(0x1001, WatchpointKind::Read);

        map.check_read_watch(0x1000, 0xAA);
        map.check_read_watch(0x1001, 0xBB); // queued behind the first

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
        let mut map = MemoryMap::new();
        map.region(RAM, "Work RAM", 0x0000, 0x8000, AccessKind::ReadWrite);
        map.set_watchpoint(0x1234, WatchpointKind::Write);

        map.check_write_watch(0x1234, 0x42);
        assert_eq!(map.take_hit().unwrap().region, Some("Work RAM"));
    }

    // -----------------------------------------------------------------------
    // Backing memory tests
    // -----------------------------------------------------------------------

    #[test]
    fn region_allocates_backing_for_rw() {
        let mut map = MemoryMap::new();
        map.region(RAM, "RAM", 0x0000, 0x0400, AccessKind::ReadWrite);

        assert_eq!(map.region_data(RAM).len(), 0x0400);
        assert!(map.region_data(RAM).iter().all(|&b| b == 0));
    }

    #[test]
    fn region_allocates_backing_for_rom() {
        let mut map = MemoryMap::new();
        map.region(ROM, "ROM", 0xD000, 0x3000, AccessKind::ReadOnly);

        assert_eq!(map.region_data(ROM).len(), 0x3000);
    }

    #[test]
    fn io_region_has_no_backing() {
        let mut map = MemoryMap::new();
        map.region(IO, "I/O", 0xC000, 0x100, AccessKind::Io);

        assert!(map.debug_read(0xC042).is_none());
    }

    #[test]
    fn debug_read_returns_backing_data() {
        let mut map = MemoryMap::new();
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
    fn debug_write_updates_backing() {
        let mut map = MemoryMap::new();
        map.region(RAM, "RAM", 0x0000, 0x8000, AccessKind::ReadWrite)
            .region(IO, "I/O", 0xC000, 0x100, AccessKind::Io);

        map.debug_write(0x1234, 0x42);
        assert_eq!(map.debug_read(0x1234), Some(0x42));

        // I/O write is a no-op (doesn't panic)
        map.debug_write(0xC042, 0xFF);
    }

    #[test]
    fn read_write_backing_hot_path() {
        let mut map = MemoryMap::new();
        map.region(RAM, "RAM", 0x0000, 0x8000, AccessKind::ReadWrite);

        map.write_backing(0x4000, 0xBE);
        assert_eq!(map.read_backing(0x4000), 0xBE);
    }

    #[test]
    fn load_region_copies_data() {
        let mut map = MemoryMap::new();
        map.region(ROM, "ROM", 0xD000, 0x0400, AccessKind::ReadOnly);

        let rom_data: Vec<u8> = (0..0x0400).map(|i| (i & 0xFF) as u8).collect();
        map.load_region(ROM, &rom_data);

        assert_eq!(map.debug_read(0xD000), Some(0x00));
        assert_eq!(map.debug_read(0xD0FF), Some(0xFF));
        assert_eq!(map.debug_read(0xD100), Some(0x00));
    }

    #[test]
    fn load_region_at_partial() {
        let mut map = MemoryMap::new();
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

        let mut map = MemoryMap::new();
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
        let mut map = MemoryMap::new();
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

        let mut map = MemoryMap::new();
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
        let mut map = MemoryMap::new();
        map.region(RAM, "RAM", 0x0000, 0x8000, AccessKind::ReadWrite)
            .region(ROM, "ROM", 0xD000, 0x3000, AccessKind::ReadOnly);

        // Both get independent backing
        map.region_data_mut(RAM)[0] = 0x11;
        map.region_data_mut(ROM)[0] = 0x22;

        assert_eq!(map.debug_read(0x0000), Some(0x11));
        assert_eq!(map.debug_read(0xD000), Some(0x22));
    }
}
