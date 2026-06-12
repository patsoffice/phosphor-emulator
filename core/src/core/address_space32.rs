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

use crate::core::memory_map::{AccessKind, RegionId};

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
