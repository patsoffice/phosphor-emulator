//! Shared address-space services and debug-result types.
//!
//! The pieces common to the parallel 16-bit and 32-bit address-space
//! implementations ([`AddressSpace16`](crate::core::address_space16::AddressSpace16),
//! [`AddressSpace32`](crate::core::address_space32::AddressSpace32)):
//! region identity ([`RegionId`], [`AccessKind`]), byte storage
//! ([`MemoryBacking`]), and the canonical side-effect-free memory-access
//! result types ([`DebugRead`], [`DebugWrite`]) consumed by the debugger.
//!
//! Decode stays concrete and per-width in the sibling modules; only the
//! width-independent services live here. See
//! `docs/designs/address-space-refactor.md`.

/// Machine-defined region identifier. Values are assigned by each machine
/// as constants (e.g., `const VIDEO_RAM: RegionId = 1`). The address space
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

/// Named byte storage for RAM/ROM/NVRAM/inactive banks, indexed by region.
///
/// Owns a flat `Vec<u8>` plus per-region offset/length tables. It knows
/// nothing about address decode; callers resolve an address to a
/// `(region_id, region-local offset)` pair first (via `AddressMap16` or
/// `AddressMap32`) and read or write through that. Shared by the 16-bit
/// and 32-bit address spaces.
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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
