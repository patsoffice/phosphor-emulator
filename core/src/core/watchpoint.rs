//! Shared watchpoint machinery for address spaces and debug tooling.
//!
//! Watchpoints are debug/observation state, not address decode or memory
//! storage. Keeping them in their own module lets page-table address spaces,
//! manual `BusDebug` implementations, and (later) device-register watchpoints
//! reuse the same machinery.
//!
//! These are the canonical shared debug types described in
//! `docs/designs/address-space-refactor.md` and consumed by
//! `docs/designs/debug-observability.md`:
//!
//! - [`WatchpointHit`] — full hit metadata (queued, not single-slot)
//! - [`DebugAccessSource`] — debug-facing "who did the access" enum
//! - [`Watchpoints`] — watchpoint set plus a FIFO hit queue
//!
//! `check_read`/`check_write` accept the observability metadata (`cycle`,
//! `pc`) from the caller and derive `phase` from the access kind; callers
//! that also know `region`/`device` names build the hit themselves and
//! queue it with [`Watchpoints::push_hit`].

use std::collections::VecDeque;

use crate::core::bus::BusMaster;

/// Whether a watchpoint triggers on reads, writes, or both.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WatchpointKind {
    Read,
    Write,
}

/// An extra predicate on a watchpoint's accessed value.
///
/// [`Always`](Self::Always) preserves the plain "fire on any access"
/// behavior; the other variants let a watchpoint fire only on a specific
/// value, a bit pattern, or a change — so a headless hunt can say "break when
/// `0x4E5F` is written" or "break when this variable changes" without
/// wading through every access. The comparison uses the low `width * 8` bits
/// of the accessed value (see [`Watchpoint::evaluate`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchpointCondition {
    /// Fire on every access (the default; matches unconditional watchpoints).
    Always,
    /// Fire when `(value & width_mask) == v`.
    Equals(u32),
    /// Fire when `(value & mask) == expected` (bit tests / any-of ranges).
    Bits { mask: u32, expected: u32 },
    /// Fire when the value differs from the last one observed at this
    /// address/kind (the first observation counts as a change).
    Changed,
}

/// Mask selecting the low `width` bytes of an accessed value.
///
/// Watchpoint values carry the low `width * 8` significant bits; width 0 (no
/// value) and widths ≥ 4 use the full 32-bit value.
#[inline]
fn width_mask(width: u8) -> u32 {
    match width {
        1 => 0xFF,
        2 => 0xFFFF,
        3 => 0x00FF_FFFF,
        _ => 0xFFFF_FFFF,
    }
}

/// Whether a hit was recorded before or after the access took effect.
///
/// Write hits are recorded `Before` the side effect is applied: boards
/// check write watchpoints at the top of bus dispatch, so the hit (and the
/// debugger pause at the end of the cycle) reflects metadata captured
/// pre-mutation. Read hits are recorded `After` the access because the
/// value is only known once the read completes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WatchpointPhase {
    Before,
    After,
}

/// Debug-facing source of a memory access.
///
/// Shared by watchpoints, debug events, and manual `BusDebug`
/// implementations — used instead of the core-specific [`BusMaster`] so
/// generic watchpoint/event code need not depend on it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DebugAccessSource {
    /// Access performed by a CPU (by index).
    Cpu(usize),
    /// Access performed by DMA.
    Dma,
    /// Access performed by a named device.
    Device(&'static str),
    /// Access performed by the frontend (memory viewer poke, etc.).
    Frontend,
    /// Source not recorded.
    Unknown,
}

impl From<BusMaster> for DebugAccessSource {
    fn from(master: BusMaster) -> Self {
        match master {
            BusMaster::Cpu(i) => DebugAccessSource::Cpu(i),
            BusMaster::Dma | BusMaster::DmaVram => DebugAccessSource::Dma,
        }
    }
}

/// A single watchpoint: an exact address in one CPU's address space,
/// optionally gated by a value [`condition`](WatchpointCondition).
#[derive(Clone, Copy, Debug)]
pub struct Watchpoint {
    /// Index of the CPU whose address space is watched.
    pub cpu_index: usize,
    /// Exact address to watch.
    pub addr: u32,
    /// Trigger on reads or writes.
    pub kind: WatchpointKind,
    /// Extra value predicate; [`WatchpointCondition::Always`] fires on any access.
    pub condition: WatchpointCondition,
    /// Last value observed at this address/kind (state for
    /// [`WatchpointCondition::Changed`]). Not part of identity.
    last_value: Option<u32>,
}

/// Identity ignores `last_value` (runtime state), so a watchpoint's set/clear
/// identity is stable as its `Changed` history accumulates. The condition *is*
/// part of identity, so the same address can carry distinct conditions.
impl PartialEq for Watchpoint {
    fn eq(&self, other: &Self) -> bool {
        self.cpu_index == other.cpu_index
            && self.addr == other.addr
            && self.kind == other.kind
            && self.condition == other.condition
    }
}

impl Watchpoint {
    /// Evaluate this watchpoint's condition against an access `value` of
    /// `width` bytes, returning whether it should fire. Updates the `Changed`
    /// history as a side effect, so it must be called on every matching
    /// access (not short-circuited away).
    fn evaluate(&mut self, value: u32, width: u8) -> bool {
        let v = value & width_mask(width);
        match self.condition {
            WatchpointCondition::Always => true,
            WatchpointCondition::Equals(target) => v == target,
            WatchpointCondition::Bits { mask, expected } => (value & mask) == expected,
            WatchpointCondition::Changed => {
                let changed = self.last_value != Some(v);
                self.last_value = Some(v);
                changed
            }
        }
    }
}

/// Details of a watchpoint hit, consumed by the debugger.
///
/// Canonical shape shared between the address-space refactor and
/// debug-observability. Instrumented boards populate all fields; boards
/// not yet migrated leave the unknown metadata at its defaults (`cycle` 0,
/// `pc`/`region`/`device` `None`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WatchpointHit {
    /// Index of the CPU whose address space the watchpoint lives in.
    pub cpu_index: usize,
    /// Who performed the access.
    pub source: DebugAccessSource,
    /// Machine cycle of the access (0 until instrumented).
    pub cycle: u64,
    /// Program counter at the access, when known.
    pub pc: Option<u32>,
    /// Address that fired.
    pub addr: u32,
    /// Read or write.
    pub kind: WatchpointKind,
    /// Recorded before or after the access took effect.
    pub phase: WatchpointPhase,
    /// Value read or written (low `width * 8` bits significant).
    pub value: u32,
    /// Access width in bytes (1, 2, or 4).
    pub width: u8,
    /// Name of the mapped region containing `addr`, when known.
    pub region: Option<&'static str>,
    /// Name of the device that owns the address, when known.
    pub device: Option<&'static str>,
}

/// Maximum queued hits before the oldest are dropped.
///
/// Bounds memory if the debugger stops polling while a hot address is
/// watched. Newest hits win, matching the old single-slot behavior.
const MAX_PENDING_HITS: usize = 64;

/// A set of exact-address watchpoints plus a FIFO queue of hits.
///
/// Hits are queued rather than overwritten, so multiple maps (or multiple
/// accesses between debugger polls) cannot lose each other's hits.
///
/// This struct does address-level matching only. Page-table address spaces
/// keep their own per-page fast filter and consult this set only on
/// flagged pages.
#[derive(Debug, Default)]
pub struct Watchpoints {
    watched: Vec<Watchpoint>,
    pending_hits: VecDeque<WatchpointHit>,
}

impl Watchpoints {
    /// Create an empty watchpoint set.
    pub fn new() -> Self {
        Self::default()
    }

    /// True if no watchpoints are set.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.watched.is_empty()
    }

    /// All currently set watchpoints.
    pub fn watched(&self) -> &[Watchpoint] {
        &self.watched
    }

    /// Set an unconditional watchpoint on the exact address `addr` in
    /// `cpu_index`'s address space. Duplicates are ignored.
    ///
    /// Shim for [`set_cond`](Self::set_cond) with
    /// [`WatchpointCondition::Always`], so unconditional callers are untouched.
    pub fn set(&mut self, cpu_index: usize, addr: u32, kind: WatchpointKind) {
        self.set_cond(cpu_index, addr, kind, WatchpointCondition::Always);
    }

    /// Set a watchpoint gated by `condition` on the exact address `addr` in
    /// `cpu_index`'s address space. Duplicates (same cpu/addr/kind/condition)
    /// are ignored.
    pub fn set_cond(
        &mut self,
        cpu_index: usize,
        addr: u32,
        kind: WatchpointKind,
        condition: WatchpointCondition,
    ) {
        let wp = Watchpoint {
            cpu_index,
            addr,
            kind,
            condition,
            last_value: None,
        };
        if !self.watched.contains(&wp) {
            self.watched.push(wp);
        }
    }

    /// Clear all watchpoints on the exact address `addr`/`kind` in
    /// `cpu_index`'s address space (regardless of condition).
    pub fn clear(&mut self, cpu_index: usize, addr: u32, kind: WatchpointKind) {
        self.watched
            .retain(|w| !(w.cpu_index == cpu_index && w.addr == addr && w.kind == kind));
    }

    /// Clear all watchpoints and drop any queued hits.
    pub fn clear_all(&mut self) {
        self.watched.clear();
        self.pending_hits.clear();
    }

    /// Check a read access against the set. Queues a hit and returns true
    /// if `addr` is read-watched in `cpu_index`'s address space.
    ///
    /// Call after the value is read; the hit records [`WatchpointPhase::After`].
    /// `cycle` is the machine clock at the access and `pc` the address of
    /// the instruction performing it, when known (pass 0 / `None` otherwise).
    #[allow(clippy::too_many_arguments)]
    #[inline]
    pub fn check_read(
        &mut self,
        cpu_index: usize,
        source: DebugAccessSource,
        cycle: u64,
        pc: Option<u32>,
        addr: u32,
        value: u32,
        width: u8,
    ) -> bool {
        self.check(
            cpu_index,
            source,
            cycle,
            pc,
            addr,
            WatchpointKind::Read,
            value,
            width,
        )
    }

    /// Check a write access against the set. Queues a hit and returns true
    /// if `addr` is write-watched in `cpu_index`'s address space.
    ///
    /// Call before applying the side effect; the hit records
    /// [`WatchpointPhase::Before`]. `cycle` is the machine clock at the
    /// access and `pc` the address of the instruction performing it, when
    /// known (pass 0 / `None` otherwise).
    #[allow(clippy::too_many_arguments)]
    #[inline]
    pub fn check_write(
        &mut self,
        cpu_index: usize,
        source: DebugAccessSource,
        cycle: u64,
        pc: Option<u32>,
        addr: u32,
        value: u32,
        width: u8,
    ) -> bool {
        self.check(
            cpu_index,
            source,
            cycle,
            pc,
            addr,
            WatchpointKind::Write,
            value,
            width,
        )
    }

    /// True if the exact address is watched for `kind` in `cpu_index`'s
    /// address space, ignoring any value condition. A pure existence
    /// predicate; use [`fires`](Self::fires) to also evaluate conditions.
    #[inline]
    pub fn matches(&self, cpu_index: usize, addr: u32, kind: WatchpointKind) -> bool {
        self.watched
            .iter()
            .any(|w| w.cpu_index == cpu_index && w.addr == addr && w.kind == kind)
    }

    /// True if any watchpoint on `(cpu_index, addr, kind)` fires for this
    /// access, evaluating each one's [`WatchpointCondition`]. Does not queue a
    /// hit — callers that populate richer metadata (e.g. region names) call
    /// this, then build the hit and [`push_hit`] it.
    ///
    /// Takes `&mut self` because [`WatchpointCondition::Changed`] tracks the
    /// last observed value. Every matching watchpoint is evaluated (not
    /// short-circuited) so `Changed` history stays consistent.
    ///
    /// [`push_hit`]: Self::push_hit
    #[inline]
    pub fn fires(
        &mut self,
        cpu_index: usize,
        addr: u32,
        kind: WatchpointKind,
        value: u32,
        width: u8,
    ) -> bool {
        let mut any = false;
        for w in self.watched.iter_mut() {
            if w.cpu_index == cpu_index
                && w.addr == addr
                && w.kind == kind
                && w.evaluate(value, width)
            {
                any = true;
            }
        }
        any
    }

    #[allow(clippy::too_many_arguments)]
    fn check(
        &mut self,
        cpu_index: usize,
        source: DebugAccessSource,
        cycle: u64,
        pc: Option<u32>,
        addr: u32,
        kind: WatchpointKind,
        value: u32,
        width: u8,
    ) -> bool {
        if !self.fires(cpu_index, addr, kind, value, width) {
            return false;
        }
        self.push_hit(WatchpointHit {
            cpu_index,
            source,
            cycle,
            pc,
            addr,
            kind,
            phase: match kind {
                WatchpointKind::Read => WatchpointPhase::After,
                WatchpointKind::Write => WatchpointPhase::Before,
            },
            value,
            width,
            region: None,
            device: None,
        });
        true
    }

    /// Queue an externally constructed hit (for boards that populate
    /// richer metadata than `check_read`/`check_write`).
    pub fn push_hit(&mut self, hit: WatchpointHit) {
        if self.pending_hits.len() >= MAX_PENDING_HITS {
            self.pending_hits.pop_front();
        }
        self.pending_hits.push_back(hit);
    }

    /// Consume the oldest queued hit (polled by the debugger).
    #[inline]
    pub fn take_hit(&mut self) -> Option<WatchpointHit> {
        self.pending_hits.pop_front()
    }

    /// Number of hits waiting to be consumed.
    pub fn pending_hits(&self) -> usize {
        self.pending_hits.len()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_set_matches_nothing() {
        let mut wp = Watchpoints::new();
        assert!(wp.is_empty());
        assert!(!wp.check_read(0, DebugAccessSource::Unknown, 0, None, 0x1234, 0x42, 1));
        assert!(!wp.check_write(0, DebugAccessSource::Unknown, 0, None, 0x1234, 0x42, 1));
        assert!(wp.take_hit().is_none());
    }

    #[test]
    fn exact_address_matching() {
        let mut wp = Watchpoints::new();
        wp.set(0, 0x2042, WatchpointKind::Write);

        // Nearby addresses do not fire
        assert!(!wp.check_write(0, DebugAccessSource::Unknown, 0, None, 0x2041, 0x11, 1));
        assert!(!wp.check_write(0, DebugAccessSource::Unknown, 0, None, 0x2043, 0x22, 1));

        // Exact address fires
        assert!(wp.check_write(0, DebugAccessSource::Unknown, 0, None, 0x2042, 0x55, 1));
        let hit = wp.take_hit().unwrap();
        assert_eq!(hit.addr, 0x2042);
        assert_eq!(hit.value, 0x55);
        assert_eq!(hit.kind, WatchpointKind::Write);
        assert_eq!(hit.phase, WatchpointPhase::Before);
    }

    #[test]
    fn read_and_write_are_separate() {
        let mut wp = Watchpoints::new();
        wp.set(0, 0x4000, WatchpointKind::Read);

        assert!(!wp.check_write(0, DebugAccessSource::Unknown, 0, None, 0x4000, 0xCD, 1));
        assert!(wp.check_read(0, DebugAccessSource::Unknown, 0, None, 0x4000, 0xAB, 1));
        let hit = wp.take_hit().unwrap();
        assert_eq!(hit.kind, WatchpointKind::Read);
        assert!(wp.take_hit().is_none());
    }

    #[test]
    fn cpu_index_scopes_watchpoints() {
        let mut wp = Watchpoints::new();
        wp.set(1, 0x4000, WatchpointKind::Read);

        // Same address in CPU 0's space does not fire
        assert!(!wp.check_read(0, DebugAccessSource::Unknown, 0, None, 0x4000, 0x00, 1));
        // CPU 1's space fires
        assert!(wp.check_read(1, DebugAccessSource::Unknown, 0, None, 0x4000, 0x77, 1));
        let hit = wp.take_hit().unwrap();
        assert_eq!(hit.cpu_index, 1);
    }

    #[test]
    fn multiple_hits_are_queued_in_order() {
        let mut wp = Watchpoints::new();
        wp.set(0, 0x1000, WatchpointKind::Read);
        wp.set(0, 0x1001, WatchpointKind::Read);

        assert!(wp.check_read(0, DebugAccessSource::Unknown, 0, None, 0x1000, 0xAA, 1));
        assert!(wp.check_read(0, DebugAccessSource::Unknown, 0, None, 0x1001, 0xBB, 1));
        assert_eq!(wp.pending_hits(), 2);

        // FIFO: oldest first
        assert_eq!(wp.take_hit().unwrap().value, 0xAA);
        assert_eq!(wp.take_hit().unwrap().value, 0xBB);
        assert!(wp.take_hit().is_none());
    }

    #[test]
    fn queue_drops_oldest_when_full() {
        let mut wp = Watchpoints::new();
        wp.set(0, 0x1000, WatchpointKind::Write);

        for i in 0..(MAX_PENDING_HITS as u32 + 8) {
            wp.check_write(0, DebugAccessSource::Unknown, 0, None, 0x1000, i, 1);
        }
        assert_eq!(wp.pending_hits(), MAX_PENDING_HITS);
        // Oldest hits were dropped; the first remaining is hit #8
        assert_eq!(wp.take_hit().unwrap().value, 8);
    }

    #[test]
    fn source_metadata_recorded() {
        let mut wp = Watchpoints::new();
        wp.set(0, 0x5000, WatchpointKind::Read);

        assert!(wp.check_read(0, DebugAccessSource::Cpu(0), 0, None, 0x5000, 0x01, 1));
        assert!(wp.check_read(0, DebugAccessSource::Dma, 0, None, 0x5000, 0x02, 1));

        assert_eq!(wp.take_hit().unwrap().source, DebugAccessSource::Cpu(0));
        assert_eq!(wp.take_hit().unwrap().source, DebugAccessSource::Dma);
    }

    #[test]
    fn cycle_and_pc_metadata_recorded() {
        let mut wp = Watchpoints::new();
        wp.set(0, 0x5000, WatchpointKind::Read);
        wp.set(0, 0x5001, WatchpointKind::Write);

        assert!(wp.check_read(
            0,
            DebugAccessSource::Cpu(0),
            12_345,
            Some(0x1BCC),
            0x5000,
            0x01,
            1
        ));
        assert!(wp.check_write(0, DebugAccessSource::Dma, 67_890, None, 0x5001, 0x02, 1));

        let hit = wp.take_hit().unwrap();
        assert_eq!(hit.cycle, 12_345);
        assert_eq!(hit.pc, Some(0x1BCC));
        let hit = wp.take_hit().unwrap();
        assert_eq!(hit.cycle, 67_890);
        assert_eq!(hit.pc, None);
    }

    #[test]
    fn phase_follows_access_kind() {
        let mut wp = Watchpoints::new();
        wp.set(0, 0x7000, WatchpointKind::Read);
        wp.set(0, 0x7000, WatchpointKind::Write);

        // Reads record after the value is known
        assert!(wp.check_read(0, DebugAccessSource::Unknown, 0, None, 0x7000, 0xAA, 1));
        assert_eq!(wp.take_hit().unwrap().phase, WatchpointPhase::After);

        // Writes record before the side effect
        assert!(wp.check_write(0, DebugAccessSource::Unknown, 0, None, 0x7000, 0xBB, 1));
        assert_eq!(wp.take_hit().unwrap().phase, WatchpointPhase::Before);
    }

    #[test]
    fn width_metadata_recorded() {
        let mut wp = Watchpoints::new();
        wp.set(0, 0x6000, WatchpointKind::Write);

        assert!(wp.check_write(
            0,
            DebugAccessSource::Unknown,
            0,
            None,
            0x6000,
            0x1234_5678,
            4
        ));
        let hit = wp.take_hit().unwrap();
        assert_eq!(hit.value, 0x1234_5678);
        assert_eq!(hit.width, 4);
    }

    #[test]
    fn clear_by_address_and_kind() {
        let mut wp = Watchpoints::new();
        wp.set(0, 0x3000, WatchpointKind::Read);
        wp.set(0, 0x3000, WatchpointKind::Write);

        wp.clear(0, 0x3000, WatchpointKind::Read);
        assert!(!wp.check_read(0, DebugAccessSource::Unknown, 0, None, 0x3000, 0x00, 1));
        assert!(wp.check_write(0, DebugAccessSource::Unknown, 0, None, 0x3000, 0x00, 1));

        wp.take_hit();
        wp.clear(0, 0x3000, WatchpointKind::Write);
        assert!(wp.is_empty());
    }

    #[test]
    fn clear_all_drops_watchpoints_and_hits() {
        let mut wp = Watchpoints::new();
        wp.set(0, 0x1000, WatchpointKind::Read);
        wp.set(0, 0x2000, WatchpointKind::Write);
        wp.check_read(0, DebugAccessSource::Unknown, 0, None, 0x1000, 0xAA, 1);

        wp.clear_all();
        assert!(wp.is_empty());
        assert!(wp.take_hit().is_none());
        assert!(!wp.check_read(0, DebugAccessSource::Unknown, 0, None, 0x1000, 0x00, 1));
    }

    #[test]
    fn duplicate_set_is_ignored() {
        let mut wp = Watchpoints::new();
        wp.set(0, 0x1000, WatchpointKind::Read);
        wp.set(0, 0x1000, WatchpointKind::Read);
        assert_eq!(wp.watched().len(), 1);
    }

    #[test]
    fn bus_master_converts_to_source() {
        assert_eq!(
            DebugAccessSource::from(BusMaster::Cpu(2)),
            DebugAccessSource::Cpu(2)
        );
        assert_eq!(
            DebugAccessSource::from(BusMaster::Dma),
            DebugAccessSource::Dma
        );
        assert_eq!(
            DebugAccessSource::from(BusMaster::DmaVram),
            DebugAccessSource::Dma
        );
    }

    // ---- Conditional watchpoints -------------------------------------------

    /// A bare `set` remains unconditional (`Always`): every access fires.
    #[test]
    fn always_condition_is_the_plain_default() {
        let mut wp = Watchpoints::new();
        wp.set(0, 0x1000, WatchpointKind::Write);
        assert_eq!(wp.watched()[0].condition, WatchpointCondition::Always);
        assert!(wp.check_write(0, DebugAccessSource::Unknown, 0, None, 0x1000, 0x11, 1));
        assert!(wp.check_write(0, DebugAccessSource::Unknown, 0, None, 0x1000, 0x22, 1));
    }

    #[test]
    fn equals_fires_only_on_matching_value() {
        let mut wp = Watchpoints::new();
        wp.set_cond(
            0,
            0x1000,
            WatchpointKind::Write,
            WatchpointCondition::Equals(0x4E5F),
        );
        // Word write of the target value fires.
        assert!(wp.check_write(0, DebugAccessSource::Unknown, 0, None, 0x1000, 0x4E5F, 2));
        // Any other value does not.
        assert!(!wp.check_write(0, DebugAccessSource::Unknown, 0, None, 0x1000, 0x4E60, 2));
        // Width-masking: a byte access can never equal a 16-bit target.
        assert!(!wp.check_write(0, DebugAccessSource::Unknown, 0, None, 0x1000, 0x5F, 1));
    }

    #[test]
    fn bits_fires_on_masked_pattern() {
        let mut wp = Watchpoints::new();
        wp.set_cond(
            0,
            0x2000,
            WatchpointKind::Write,
            WatchpointCondition::Bits {
                mask: 0xF0,
                expected: 0x50,
            },
        );
        assert!(wp.check_write(0, DebugAccessSource::Unknown, 0, None, 0x2000, 0x5A, 1)); // 0x5A & F0 = 50
        assert!(wp.check_write(0, DebugAccessSource::Unknown, 0, None, 0x2000, 0x51, 1)); // 0x51 & F0 = 50
        assert!(!wp.check_write(0, DebugAccessSource::Unknown, 0, None, 0x2000, 0x60, 1)); // 60 & F0 = 60
    }

    #[test]
    fn changed_fires_on_first_and_on_change_only() {
        let mut wp = Watchpoints::new();
        wp.set_cond(
            0,
            0x3000,
            WatchpointKind::Write,
            WatchpointCondition::Changed,
        );
        // First observation counts as a change.
        assert!(wp.check_write(0, DebugAccessSource::Unknown, 0, None, 0x3000, 0x10, 1));
        // Same value again: no change.
        assert!(!wp.check_write(0, DebugAccessSource::Unknown, 0, None, 0x3000, 0x10, 1));
        // Different value: fires, and updates the baseline.
        assert!(wp.check_write(0, DebugAccessSource::Unknown, 0, None, 0x3000, 0x20, 1));
        assert!(!wp.check_write(0, DebugAccessSource::Unknown, 0, None, 0x3000, 0x20, 1));
    }

    #[test]
    fn condition_is_part_of_identity_for_dedup() {
        let mut wp = Watchpoints::new();
        wp.set_cond(
            0,
            0x1000,
            WatchpointKind::Write,
            WatchpointCondition::Equals(1),
        );
        wp.set_cond(
            0,
            0x1000,
            WatchpointKind::Write,
            WatchpointCondition::Equals(1),
        ); // dup
        wp.set_cond(
            0,
            0x1000,
            WatchpointKind::Write,
            WatchpointCondition::Equals(2),
        ); // distinct
        assert_eq!(wp.watched().len(), 2);
        // clear() removes every condition at that addr/kind.
        wp.clear(0, 0x1000, WatchpointKind::Write);
        assert!(wp.is_empty());
    }

    #[test]
    fn fires_evaluates_every_matching_changed_watchpoint() {
        // Two Changed watchpoints on the same addr/kind: both must have their
        // baseline updated on every access, or one would fire spuriously.
        let mut wp = Watchpoints::new();
        wp.set_cond(
            0,
            0x1000,
            WatchpointKind::Write,
            WatchpointCondition::Changed,
        );
        // A second, distinct condition on the same address keeps its own state.
        wp.set_cond(
            0,
            0x1000,
            WatchpointKind::Write,
            WatchpointCondition::Equals(0x99),
        );
        assert!(wp.fires(0, 0x1000, WatchpointKind::Write, 0x10, 1)); // Changed: first-touch
        assert!(!wp.fires(0, 0x1000, WatchpointKind::Write, 0x10, 1)); // Changed baseline held
        assert!(wp.fires(0, 0x1000, WatchpointKind::Write, 0x99, 1)); // Equals(0x99) fires
    }
}
