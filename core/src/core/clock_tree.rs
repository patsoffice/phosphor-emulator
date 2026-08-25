//! Board clock trees: one declared crystal set per board, every sub-clock
//! derived from it.
//!
//! An arcade board is a clock tree. A crystal is divided down to the CPU, the
//! video dot clock, the sound CPU and the chip clocks hanging off them; the
//! scanline count and the sound-chip rates are consequences of that division.
//! [`ClockTree`] is where a board states that division once, in a type, instead
//! of restating each leaf as a separately-rounded constant.
//!
//! # The tree does bookkeeping, not scheduling
//!
//! A tree does **not** own the frame loop, and it does not step at the master
//! rate. A board still runs one iteration per CPU cycle and steps the two or
//! three domains it has, each with the same single add-and-compare
//! [`ClockDivider`](super::clock::ClockDivider) performs today. The ratio to
//! the crystal is retained for [`hz`](ClockTree::hz), for validation and for
//! the debugger, never for stepping. Every domain therefore carries two
//! ratios:
//!
//! * `root_num/root_den`: the ratio to its crystal. This is the auditable
//!   hardware statement: `add_domain(Cpu, root, 1, 8)` reads as "the CPU is the
//!   crystal over eight".
//! * `step_num/step_den`: the ratio to the *stepping* domain (the CPU),
//!   precomputed from the above by [`set_step_domain`](ClockTree::set_step_domain).
//!   This is what [`advance`](ClockTree::advance) and [`tick`](ClockTree::tick)
//!   consume, so a step costs exactly what `ClockDivider::tick` costs.
//!
//! Both reductions are exact: every rate is a rational multiple of an
//! integer-Hz crystal, so dividing one by the other reduces by `gcd` without
//! loss.
//!
//! # Multiple crystals live in one tree
//!
//! A board whose video clock is on a different crystal from its CPU holds both
//! roots in one tree, because the relationship between them is exactly the
//! rounding worth auditing. [`cycles_per_scanline`](ClockTree::cycles_per_scanline)
//! is the one place that conversion happens, and it returns the rounding error
//! in ppm alongside the integer count so a board can state the error it accepts
//! rather than burying it in a comment.
//!
//! ```
//! use phosphor_core::core::clock_tree::{ClockDomainName as N, ClockTree, RootId};
//!
//! // Do! Castle: both Z80s on a 4 MHz crystal, video on its own 9.828 MHz one.
//! let mut t = ClockTree::new(4_000_000);
//! let vid = t.add_root(9_828_000);
//! let cpu = t.add_domain(N::Cpu, RootId::MAIN, 1, 1);
//! let dot = t.add_domain(N::Pixel, vid, 1, 2);
//! t.add_domain(N::Psg, RootId::MAIN, 1, 16);
//! t.set_step_domain(cpu);
//!
//! assert_eq!(t.hz(dot), 4_914_000);
//! // HTOTAL 312 dot-clocks is 253.968… CPU cycles; the board runs 254 of them,
//! // which makes its video clock 125 ppm slow.
//! assert_eq!(t.cycles_per_scanline(dot, 312), (254, -125));
//! ```
//!
//! # Save state
//!
//! [`ClockDomain`] saves `step_num`, `step_den` and `phase_accum`; its ratio to
//! the crystal is `#[save_skip]`ped and re-provided by the board's declaration.
//! That split is deliberate: a domain retuned at runtime (a VCO driven by a
//! DAC, a clock-select bit) must reload *retuned*, which means the live ratio
//! has to travel in the save file while the declared hardware ratio does not.

use phosphor_macros::Saveable;

/// Maximum crystals one board may declare.
pub const MAX_ROOTS: usize = 4;

/// Maximum clock domains one board may declare.
///
/// Generous: the most any board currently divides out of its crystals is three.
pub const MAX_DOMAINS: usize = 8;

// ---------------------------------------------------------------------------
// Handles
// ---------------------------------------------------------------------------

/// Stable handle into a tree's domain table.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct DomainId(u8);

impl DomainId {
    /// Index into the tree's domain table.
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Which crystal a domain hangs off.
///
/// Most boards have one. Do! Castle, Mr. Do!, Mario Bros., Congo Bongo,
/// Scramble, TKG-04 and Gottlieb System 80 have two or three.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct RootId(u8);

impl RootId {
    /// The crystal passed to [`ClockTree::new`].
    pub const MAIN: RootId = RootId(0);

    /// Index into the tree's root table.
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// What a domain drives.
///
/// A fieldless tag rather than a `&'static str`, so the set of things a board
/// can declare stays enumerable and a typo is a compile error. Extend it when a
/// board needs a domain this list cannot name.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ClockDomainName {
    /// Unused table slot. Never fires.
    Unused,
    /// The main CPU.
    Cpu,
    /// A second CPU sharing the main CPU's role (multi-CPU boards).
    Cpu2,
    /// A third CPU sharing the main CPU's role.
    Cpu3,
    /// A subordinate CPU driving video or I/O rather than sound.
    SubCpu,
    /// A dedicated sound CPU.
    SoundCpu,
    /// An embedded microcontroller (I8035/I8039/MB88xx custom).
    Mcu,
    /// The video dot clock.
    Pixel,
    /// A programmable sound generator (SN76489, AY-3-8910, Namco WSG).
    Psg,
    /// A second sound generator on the same board.
    Psg2,
    /// A POKEY.
    Pokey,
    /// A speech synthesiser (TMS5220, Votrax SC-01).
    Speech,
    /// A DAC or discrete audio stage clocked off the board.
    Dac,
    /// A vector generator (DVG, AVG).
    Vector,
}

impl ClockDomainName {
    /// Short label for debug views.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unused => "unused",
            Self::Cpu => "cpu",
            Self::Cpu2 => "cpu2",
            Self::Cpu3 => "cpu3",
            Self::SubCpu => "subcpu",
            Self::SoundCpu => "soundcpu",
            Self::Mcu => "mcu",
            Self::Pixel => "pixel",
            Self::Psg => "psg",
            Self::Psg2 => "psg2",
            Self::Pokey => "pokey",
            Self::Speech => "speech",
            Self::Dac => "dac",
            Self::Vector => "vector",
        }
    }
}

// ---------------------------------------------------------------------------
// Rational helpers
// ---------------------------------------------------------------------------

fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

/// Reduce `num/den` to lowest terms and narrow it to a `u32` pair.
///
/// Panics rather than rounding: a ratio that will not fit is a board
/// declaration that cannot be represented exactly, and silently approximating
/// it is the bug class this module exists to remove.
fn reduce_to_u32(num: u128, den: u128, what: &str) -> (u32, u32) {
    assert!(den != 0, "{what}: denominator is zero");
    let g = gcd(num, den).max(1);
    let (n, d) = (num / g, den / g);
    assert!(
        n <= u32::MAX as u128 && d <= u32::MAX as u128,
        "{what}: ratio {n}/{d} does not fit in u32 after reduction: the two \
         crystals involved are close to coprime, so no exact Bresenham ratio \
         between them is representable"
    );
    // `advance` adds `step_num` to a `phase_accum` that is always below
    // `step_den`, so their sum must not wrap.
    assert!(
        n + d <= u32::MAX as u128,
        "{what}: ratio {n}/{d} would overflow the phase accumulator"
    );
    (n as u32, d as u32)
}

// ---------------------------------------------------------------------------
// ClockDomain
// ---------------------------------------------------------------------------

/// One clock in a board's tree.
///
/// Constructed only through [`ClockTree::add_domain`]; a bare `ClockDomain` has
/// no way to name its crystal.
#[derive(Saveable, Copy, Clone, Debug)]
#[save_version(1)]
pub struct ClockDomain {
    #[save_skip]
    name: ClockDomainName,
    #[save_skip]
    root: RootId,
    /// Ratio to `root`'s crystal: the auditable hardware statement.
    #[save_skip]
    root_num: u32,
    #[save_skip]
    root_den: u32,
    /// Ratio to the stepping domain, precomputed from the above.
    ///
    /// Saved, not skipped: a domain retuned at runtime must reload retuned.
    step_num: u32,
    step_den: u32,
    phase_accum: u32,
}

impl ClockDomain {
    /// An inert table slot: never fires, and `step_den = 1` keeps the compare
    /// in [`advance`] well-defined.
    const INERT: ClockDomain = ClockDomain {
        name: ClockDomainName::Unused,
        root: RootId::MAIN,
        root_num: 0,
        root_den: 1,
        step_num: 0,
        step_den: 1,
        phase_accum: 0,
    };

    /// Advance one stepping-domain cycle; return how many times this domain
    /// fired.
    ///
    /// Normally 0 or 1. It is greater than 1 when the domain outruns the
    /// stepping domain, as Congo Bongo's 4 MHz sound Z80 does against its
    /// 3.04125 MHz main CPU.
    #[inline]
    pub fn advance(&mut self) -> u32 {
        self.phase_accum += self.step_num;
        if self.phase_accum < self.step_den {
            return 0;
        }
        let mut fired = 0;
        while self.phase_accum >= self.step_den {
            self.phase_accum -= self.step_den;
            fired += 1;
        }
        fired
    }

    /// Advance one stepping-domain cycle; return whether this domain fired.
    ///
    /// The fast path for a domain no faster than the stepping domain, which is
    /// every board clock but Congo Bongo's sound Z80. Identical in cost to
    /// [`ClockDivider::tick`](super::clock::ClockDivider::tick).
    #[inline]
    pub fn tick(&mut self) -> bool {
        debug_assert!(
            self.step_num <= self.step_den,
            "{} outruns the stepping domain ({}/{}), so use advance()",
            self.name.as_str(),
            self.step_num,
            self.step_den
        );
        self.phase_accum += self.step_num;
        if self.phase_accum >= self.step_den {
            self.phase_accum -= self.step_den;
            true
        } else {
            false
        }
    }

    /// What this domain drives.
    pub const fn name(self) -> ClockDomainName {
        self.name
    }

    /// The crystal this domain hangs off.
    pub const fn root(self) -> RootId {
        self.root
    }

    /// Declared ratio to the crystal: the hardware statement, unaffected by
    /// any runtime retune.
    pub const fn root_ratio(self) -> (u32, u32) {
        (self.root_num, self.root_den)
    }

    /// Live ratio to the stepping domain.
    pub const fn step_ratio(self) -> (u32, u32) {
        (self.step_num, self.step_den)
    }

    /// Current phase accumulator.
    pub const fn phase(self) -> u32 {
        self.phase_accum
    }

    /// Reset the phase accumulator to zero.
    pub fn reset(&mut self) {
        self.phase_accum = 0;
    }
}

// ---------------------------------------------------------------------------
// Debug view
// ---------------------------------------------------------------------------

/// One row of a tree's debug view. See [`ClockTree::domains`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct DomainInfo {
    pub name: ClockDomainName,
    /// Live rate in Hz, rounded to nearest.
    pub hz: u64,
    /// Live ratio to the stepping domain.
    pub step_ratio: (u32, u32),
    pub phase: u32,
}

// ---------------------------------------------------------------------------
// ClockTree
// ---------------------------------------------------------------------------

/// A board's crystals and everything derived from them.
///
/// See the [module documentation](self) for what the type is and is not for.
#[derive(Saveable, Clone, Debug)]
#[save_version(1)]
pub struct ClockTree {
    #[save_skip]
    roots: [u32; MAX_ROOTS],
    #[save_skip]
    root_len: u8,
    /// Fixed-size: the save-state derive delegates array elements to
    /// `Saveable`, and there is no `impl Saveable for Option<T>`. Slots past
    /// `len` are [`ClockDomain::INERT`].
    domains: [ClockDomain; MAX_DOMAINS],
    #[save_skip]
    len: u8,
    #[save_skip]
    step: Option<DomainId>,
}

impl ClockTree {
    /// Start a tree with its main crystal, which becomes [`RootId::MAIN`].
    pub fn new(crystal_hz: u32) -> Self {
        assert!(crystal_hz != 0, "clock tree: main crystal is 0 Hz");
        let mut roots = [0u32; MAX_ROOTS];
        roots[0] = crystal_hz;
        Self {
            roots,
            root_len: 1,
            domains: [ClockDomain::INERT; MAX_DOMAINS],
            len: 0,
            step: None,
        }
    }

    /// Declare a second (or third) crystal.
    pub fn add_root(&mut self, crystal_hz: u32) -> RootId {
        assert!(crystal_hz != 0, "clock tree: crystal is 0 Hz");
        let idx = self.root_len as usize;
        assert!(
            idx < MAX_ROOTS,
            "clock tree: more than {MAX_ROOTS} crystals"
        );
        self.roots[idx] = crystal_hz;
        self.root_len += 1;
        RootId(idx as u8)
    }

    /// Declare a domain by its exact ratio to a crystal.
    ///
    /// `add_domain(Cpu, root, 1, 8)` reads as "the CPU is that crystal over
    /// eight". Call [`set_step_domain`](Self::set_step_domain) once every
    /// domain has been added.
    pub fn add_domain(
        &mut self,
        name: ClockDomainName,
        root: RootId,
        num: u32,
        den: u32,
    ) -> DomainId {
        let idx = self.len as usize;
        assert!(
            idx < MAX_DOMAINS,
            "clock tree: more than {MAX_DOMAINS} domains"
        );
        assert!(
            root.index() < self.root_len as usize,
            "clock tree: {} hangs off crystal {}, which was never declared",
            name.as_str(),
            root.index()
        );
        assert!(
            num != 0 && den != 0,
            "clock tree: {} declared with ratio {num}/{den}",
            name.as_str()
        );
        self.domains[idx] = ClockDomain {
            name,
            root,
            root_num: num,
            root_den: den,
            step_num: 0,
            step_den: 1,
            phase_accum: 0,
        };
        self.len += 1;
        let id = DomainId(idx as u8);
        // Keep the tree usable if a domain is added after the stepping domain
        // was nominated, rather than leaving it silently inert.
        if self.step.is_some() {
            self.recompute_step_ratios();
        }
        id
    }

    /// Nominate the domain the frame loop counts in (the CPU).
    ///
    /// Precomputes every domain's ratio to it. Exact: both sides are rational
    /// multiples of integer-Hz crystals.
    pub fn set_step_domain(&mut self, id: DomainId) {
        assert!(id.index() < self.len as usize, "clock tree: unknown domain");
        self.step = Some(id);
        self.recompute_step_ratios();
    }

    /// The domain the frame loop counts in, if one has been nominated.
    pub const fn step_domain(&self) -> Option<DomainId> {
        self.step
    }

    /// Number of declared domains.
    pub const fn len(&self) -> usize {
        self.len as usize
    }

    /// Whether no domain has been declared.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The crystals this board declares, in declaration order.
    pub fn roots(&self) -> &[u32] {
        &self.roots[..self.root_len as usize]
    }

    /// Find the first domain with the given name.
    ///
    /// How a test or a debug view reaches a domain it did not construct.
    pub fn find(&self, name: ClockDomainName) -> Option<DomainId> {
        self.domains[..self.len as usize]
            .iter()
            .position(|d| d.name == name)
            .map(|i| DomainId(i as u8))
    }

    /// Borrow a domain.
    pub fn domain(&self, id: DomainId) -> &ClockDomain {
        debug_assert!(id.index() < self.len as usize, "clock tree: unknown domain");
        &self.domains[id.index()]
    }

    /// Advance one stepping-domain cycle; return how many times `id` fired.
    #[inline]
    pub fn advance(&mut self, id: DomainId) -> u32 {
        self.domains[id.index()].advance()
    }

    /// Advance one stepping-domain cycle; return whether `id` fired.
    #[inline]
    pub fn tick(&mut self, id: DomainId) -> bool {
        self.domains[id.index()].tick()
    }

    /// A domain's live rate in Hz, rounded to nearest.
    ///
    /// Derived from the *live* ratio to the stepping domain, so a retuned
    /// domain (including one restored from a save file) reports the rate it
    /// is actually running at. Use [`ClockDomain::root_ratio`] for the declared
    /// hardware statement.
    pub fn hz(&self, id: DomainId) -> u64 {
        let (num, den) = self.hz_ratio(id);
        ((num + den / 2) / den) as u64
    }

    /// Retune a domain to an absolute rate.
    ///
    /// Recomputes both ratios and folds `phase_accum` into the new period, so a
    /// stale accumulator from a longer previous period cannot stall the domain.
    /// Pair it with the device's own `set_clock` at the same call site; that
    /// pairing is the whole point of routing a retune through here.
    pub fn set_domain_hz(&mut self, id: DomainId, hz: u32) {
        assert!(
            self.step != Some(id),
            "clock tree: the stepping domain cannot be retuned, because every \
             other domain's ratio is expressed against it"
        );
        assert!(hz != 0, "clock tree: retune to 0 Hz");
        let idx = id.index();
        assert!(idx < self.len as usize, "clock tree: unknown domain");
        let what = self.domains[idx].name.as_str();

        let root_hz = self.roots[self.domains[idx].root.index()] as u128;
        let (rn, rd) = reduce_to_u32(hz as u128, root_hz, what);
        self.domains[idx].root_num = rn;
        self.domains[idx].root_den = rd;

        let (sn, sd) = self.step_hz_ratio();
        // hz / (sn/sd) = hz * sd / sn
        let (step_num, step_den) = reduce_to_u32(hz as u128 * sd, sn, what);
        self.domains[idx].step_num = step_num;
        self.domains[idx].step_den = step_den;
        if self.domains[idx].phase_accum >= step_den {
            self.domains[idx].phase_accum %= step_den;
        }
    }

    /// Stepping-domain cycles per scanline implied by a video domain and an
    /// HTOTAL, with the rounding error of that integer count in ppm.
    ///
    /// The one place the cross-crystal conversion lives. The error is the error
    /// in the *video rate* the integer count implies: positive means the count
    /// makes the video clock run fast, negative that it runs slow. A board
    /// whose CPU and video clocks share a crystal in a whole ratio gets 0.
    ///
    /// Panics if no stepping domain has been nominated: the count is
    /// meaningless without one.
    pub fn cycles_per_scanline(&self, video: DomainId, htotal: u32) -> (u64, i32) {
        assert!(
            self.step.is_some(),
            "clock tree: cycles_per_scanline needs a stepping domain"
        );
        let (vn, vd) = self.domains[video.index()].step_ratio();
        assert!(vn != 0, "clock tree: video domain has no rate");
        // One video cycle is vd/vn stepping cycles, so HTOTAL of them is
        // htotal * vd / vn.
        let num = htotal as u128 * vd as u128;
        let den = vn as u128;
        let count = (num + den / 2) / den;
        assert!(count != 0, "clock tree: HTOTAL {htotal} rounds to 0 cycles");

        let exact_scaled = num as i128;
        let count_scaled = (count * den) as i128;
        let diff = exact_scaled - count_scaled;
        let ppm = if diff == 0 {
            0
        } else {
            let n = diff * 1_000_000;
            let half = count_scaled / 2;
            let n = if n < 0 { n - half } else { n + half };
            n / count_scaled
        };
        (count as u64, ppm as i32)
    }

    /// Every declared domain, for `debug_registers()` and overlay stats.
    pub fn domains(&self) -> impl Iterator<Item = DomainInfo> + '_ {
        (0..self.len as usize).map(move |i| {
            let d = self.domains[i];
            DomainInfo {
                name: d.name,
                hz: self.hz(DomainId(i as u8)),
                step_ratio: d.step_ratio(),
                phase: d.phase(),
            }
        })
    }

    /// Reset every domain's phase accumulator.
    pub fn reset(&mut self) {
        for d in &mut self.domains[..self.len as usize] {
            d.reset();
        }
    }

    // -- internals ----------------------------------------------------------

    /// A domain's exact rate as a rational, in Hz.
    fn hz_ratio(&self, id: DomainId) -> (u128, u128) {
        match self.step {
            // The stepping domain defines the reference, so it can only be
            // stated against its own crystal.
            Some(step) if step != id => {
                let (sn, sd) = self.step_hz_ratio();
                let d = self.domains[id.index()];
                (sn * d.step_num as u128, sd * d.step_den as u128)
            }
            _ => self.root_hz_ratio(id),
        }
    }

    /// A domain's declared rate as a rational, in Hz.
    fn root_hz_ratio(&self, id: DomainId) -> (u128, u128) {
        let d = self.domains[id.index()];
        (
            self.roots[d.root.index()] as u128 * d.root_num as u128,
            d.root_den as u128,
        )
    }

    fn step_hz_ratio(&self) -> (u128, u128) {
        let step = self.step.expect("clock tree: no stepping domain");
        self.root_hz_ratio(step)
    }

    fn recompute_step_ratios(&mut self) {
        let (sn, sd) = self.step_hz_ratio();
        let step = self.step.expect("clock tree: no stepping domain");
        for i in 0..self.len as usize {
            let d = self.domains[i];
            let (dn, dd) = self.root_hz_ratio(DomainId(i as u8));
            // (dn/dd) / (sn/sd) = dn*sd / (dd*sn)
            let (num, den) = if DomainId(i as u8) == step {
                (1, 1)
            } else {
                reduce_to_u32(dn * sd, dd * sn, d.name.as_str())
            };
            self.domains[i].step_num = num;
            self.domains[i].step_den = den;
            if self.domains[i].phase_accum >= den {
                self.domains[i].phase_accum %= den;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// FrameParams
// ---------------------------------------------------------------------------

/// Where in the frame a cycle count falls.
///
/// Five boards re-derive the same two expressions from `TIMING`
/// (`frame_cycle = clock % cycles_per_frame`, then divide by
/// `cycles_per_scanline`). This is that derivation, once. It does not change
/// any loop's structure: a scanline-hoisted board calls
/// [`position`](Self::position) once per scanline, a plain-loop board once per
/// cycle, exactly as each does today.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct FrameParams {
    pub cycles_per_scanline: u64,
    pub total_scanlines: u64,
    /// First scanline of vertical blanking.
    pub vblank_line: u64,
}

impl FrameParams {
    pub const fn new(cycles_per_scanline: u64, total_scanlines: u64, vblank_line: u64) -> Self {
        Self {
            cycles_per_scanline,
            total_scanlines,
            vblank_line,
        }
    }

    pub const fn cycles_per_frame(&self) -> u64 {
        self.cycles_per_scanline * self.total_scanlines
    }

    /// The scanline a free-running cycle counter is on, and whether it sits
    /// exactly on that line's first cycle.
    pub const fn position(&self, clock: u64) -> (u16, bool) {
        let frame_cycle = clock % self.cycles_per_frame();
        let scanline = frame_cycle / self.cycles_per_scanline;
        let line_cycle = frame_cycle % self.cycles_per_scanline;
        (scanline as u16, line_cycle == 0)
    }

    /// Whether a scanline is inside vertical blanking.
    pub const fn in_vblank(&self, scanline: u64) -> bool {
        scanline >= self.vblank_line
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::ClockDomainName as N;
    use super::*;
    use crate::core::save_state::{Saveable, StateReader, StateWriter};

    /// Do! Castle: 4 MHz CPU crystal, 9.828 MHz video crystal, SN76489 at
    /// CPU/16. The tree in the module docs, built where tests can retune it.
    fn docastle() -> (ClockTree, DomainId, DomainId, DomainId) {
        let mut t = ClockTree::new(4_000_000);
        let vid = t.add_root(9_828_000);
        let cpu = t.add_domain(N::Cpu, RootId::MAIN, 1, 1);
        let dot = t.add_domain(N::Pixel, vid, 1, 2);
        let sn = t.add_domain(N::Psg, RootId::MAIN, 1, 16);
        t.set_step_domain(cpu);
        (t, cpu, dot, sn)
    }

    /// Congo Bongo: main CPU is 48.66 MHz / 16, but the sound Z80 has its own
    /// 4 MHz crystal and therefore outruns it.
    fn congo_bongo() -> (ClockTree, DomainId, DomainId) {
        let mut t = ClockTree::new(48_660_000);
        let snd_xtal = t.add_root(4_000_000);
        let cpu = t.add_domain(N::Cpu, RootId::MAIN, 1, 16);
        let snd = t.add_domain(N::SoundCpu, snd_xtal, 1, 1);
        t.set_step_domain(cpu);
        (t, cpu, snd)
    }

    #[test]
    fn one_eighth_fires_once_in_eight() {
        let (mut t, _, _, sn) = docastle();
        assert_eq!(t.domain(sn).step_ratio(), (1, 16));
        let mut fires = 0;
        for _ in 0..1600 {
            if t.tick(sn) {
                fires += 1;
            }
        }
        assert_eq!(fires, 100);
    }

    #[test]
    fn stepping_domain_fires_every_cycle() {
        let (mut t, cpu, _, _) = docastle();
        assert_eq!(t.domain(cpu).step_ratio(), (1, 1));
        for _ in 0..100 {
            assert!(t.tick(cpu));
        }
    }

    #[test]
    fn hz_is_derived_from_the_declared_crystal() {
        let (t, cpu, dot, sn) = docastle();
        assert_eq!(t.hz(cpu), 4_000_000);
        assert_eq!(t.hz(dot), 4_914_000);
        assert_eq!(t.hz(sn), 250_000);
    }

    #[test]
    fn hz_rounds_a_rate_that_is_not_a_whole_number() {
        // Gottlieb System 80 sound: 3.579545 MHz / 4 = 894886.25 Hz.
        let mut t = ClockTree::new(5_000_000);
        let snd_xtal = t.add_root(3_579_545);
        let cpu = t.add_domain(N::Cpu, RootId::MAIN, 1, 1);
        let snd = t.add_domain(N::SoundCpu, snd_xtal, 1, 4);
        t.set_step_domain(cpu);
        assert_eq!(t.hz(snd), 894_886);
    }

    #[test]
    fn a_domain_faster_than_the_stepping_domain_fires_more_than_once() {
        let (mut t, _, snd) = congo_bongo();
        // 4_000_000 / 3_041_250 reduces exactly.
        assert_eq!(t.domain(snd).step_ratio(), (3200, 2433));
        assert_eq!(t.hz(snd), 4_000_000);

        let mut total: u64 = 0;
        let mut saw_two = false;
        for step in 1..=2433u64 {
            let fired = t.advance(snd);
            // Faster than the main CPU, so it can never skip a cycle, and it
            // can never fire three times in one.
            assert!(
                fired == 1 || fired == 2,
                "step {step} fired {fired} times, but a 1.315x domain fires once \
                 or twice, never {fired}"
            );
            saw_two |= fired == 2;
            total += fired as u64;
            // Bresenham exactness: the running total tracks the ideal count
            // with no accumulated drift.
            assert_eq!(
                total,
                step * 3200 / 2433,
                "sound cycles drifted from the ideal count at step {step}"
            );
        }
        assert!(saw_two);
        assert_eq!(total, 3200);
        // A whole period of the ratio brings the phase back to zero.
        assert_eq!(t.domain(snd).phase(), 0);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "use advance()")]
    fn tick_rejects_a_domain_that_outruns_the_stepping_domain() {
        let (mut t, _, snd) = congo_bongo();
        t.tick(snd);
    }

    #[test]
    fn cycles_per_scanline_reports_the_cross_crystal_rounding() {
        // Do! Castle: 4e6 * 312 / 4.914e6 = 253.968…, run as 254.
        let (t, _, dot, _) = docastle();
        assert_eq!(t.cycles_per_scanline(dot, 312), (254, -125));
    }

    #[test]
    fn cycles_per_scanline_on_mr_do() {
        // 8.2 MHz / 2 CPU against a 19.6 MHz / 4 dot clock: 261.061…, run as
        // 261, which makes the video clock run fast rather than slow.
        let mut t = ClockTree::new(8_200_000);
        let vid = t.add_root(19_600_000);
        let cpu = t.add_domain(N::Cpu, RootId::MAIN, 1, 2);
        let dot = t.add_domain(N::Pixel, vid, 1, 4);
        t.set_step_domain(cpu);
        assert_eq!(t.hz(cpu), 4_100_000);
        assert_eq!(t.hz(dot), 4_900_000);
        assert_eq!(t.cycles_per_scanline(dot, 312), (261, 235));
    }

    #[test]
    fn cycles_per_scanline_is_exact_when_the_crystals_divide() {
        // Mario Bros.: 8 MHz / 2 CPU, 24 MHz / 4 dot clock, HTOTAL 384.
        // 384 / 6e6 * 4e6 = 256 exactly.
        let mut t = ClockTree::new(8_000_000);
        let vid = t.add_root(24_000_000);
        let cpu = t.add_domain(N::Cpu, RootId::MAIN, 1, 2);
        let dot = t.add_domain(N::Pixel, vid, 1, 4);
        t.set_step_domain(cpu);
        assert_eq!(t.cycles_per_scanline(dot, 384), (256, 0));
    }

    #[test]
    fn set_domain_hz_retunes_both_ratios_and_folds_phase() {
        let (mut t, _, _, sn) = docastle();
        // Park the accumulator somewhere inside the old period.
        for _ in 0..9 {
            t.tick(sn);
        }
        assert_eq!(t.domain(sn).phase(), 9);

        t.set_domain_hz(sn, 1_000_000);
        assert_eq!(t.hz(sn), 1_000_000);
        assert_eq!(t.domain(sn).step_ratio(), (1, 4));
        assert_eq!(t.domain(sn).root_ratio(), (1, 4));
        // 9 would have stalled a divider whose period is now 4.
        assert_eq!(t.domain(sn).phase(), 1);

        let mut fires = 0;
        for _ in 0..400 {
            if t.tick(sn) {
                fires += 1;
            }
        }
        assert_eq!(fires, 100);
    }

    #[test]
    fn a_retuned_domain_reloads_retuned() {
        // The reason step_num/step_den are saved and root_num/root_den are not:
        // a VCO retuned by a DAC write must come back at the rate it was
        // retuned to, not at the rate the board declared.
        let (mut t, _, _, sn) = docastle();
        t.set_domain_hz(sn, 720_000);
        for _ in 0..5 {
            t.tick(sn);
        }
        let phase = t.domain(sn).phase();

        let mut w = StateWriter::new();
        t.save_state(&mut w);
        let data = w.into_vec();

        // A freshly declared tree, exactly as the board would rebuild it.
        let (mut t2, _, _, sn2) = docastle();
        assert_eq!(t2.hz(sn2), 250_000);
        let mut r = StateReader::new(&data);
        t2.load_state(&mut r).unwrap();

        assert_eq!(t2.hz(sn2), 720_000);
        assert_eq!(t2.domain(sn2).step_ratio(), t.domain(sn).step_ratio());
        assert_eq!(t2.domain(sn2).phase(), phase);
        // The declared hardware ratio is deliberately not restored: it is the
        // board's statement, not machine state.
        assert_eq!(t2.domain(sn2).root_ratio(), (1, 16));
    }

    #[test]
    fn save_load_round_trips_phase_for_every_domain() {
        let (mut t, cpu, dot, sn) = docastle();
        for _ in 0..37 {
            t.tick(cpu);
            // The dot clock is 4.914 MHz against a 4 MHz CPU, so it is an
            // `advance` domain like Congo Bongo's sound Z80.
            t.advance(dot);
            t.tick(sn);
        }
        let mut w = StateWriter::new();
        t.save_state(&mut w);
        let data = w.into_vec();

        let (mut t2, ..) = docastle();
        let mut r = StateReader::new(&data);
        t2.load_state(&mut r).unwrap();
        for id in [cpu, dot, sn] {
            assert_eq!(t2.domain(id).phase(), t.domain(id).phase());
            assert_eq!(t2.domain(id).step_ratio(), t.domain(id).step_ratio());
        }
    }

    #[test]
    fn find_locates_a_domain_by_name() {
        let (t, cpu, dot, sn) = docastle();
        assert_eq!(t.find(N::Cpu), Some(cpu));
        assert_eq!(t.find(N::Pixel), Some(dot));
        assert_eq!(t.find(N::Psg), Some(sn));
        assert_eq!(t.find(N::Pokey), None);
        // Inert slots are not findable.
        assert_eq!(t.find(N::Unused), None);
    }

    #[test]
    fn domains_lists_only_declared_domains() {
        let (t, ..) = docastle();
        let rows: Vec<_> = t.domains().collect();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].name, N::Pixel);
        assert_eq!(rows[1].hz, 4_914_000);
        assert_eq!(rows[1].step_ratio, (2457, 2000));
        assert_eq!(t.roots(), &[4_000_000, 9_828_000]);
    }

    #[test]
    fn a_domain_added_after_the_step_domain_still_gets_a_ratio() {
        let (mut t, ..) = docastle();
        let late = t.add_domain(N::Speech, RootId::MAIN, 1, 5);
        assert_eq!(t.domain(late).step_ratio(), (1, 5));
        assert_eq!(t.hz(late), 800_000);
    }

    #[test]
    fn reset_clears_every_phase() {
        let (mut t, _, dot, sn) = docastle();
        for _ in 0..7 {
            t.advance(dot);
            t.tick(sn);
        }
        t.reset();
        assert_eq!(t.domain(dot).phase(), 0);
        assert_eq!(t.domain(sn).phase(), 0);
    }

    #[test]
    #[should_panic(expected = "stepping domain cannot be retuned")]
    fn the_stepping_domain_cannot_be_retuned() {
        let (mut t, cpu, _, _) = docastle();
        t.set_domain_hz(cpu, 6_000_000);
    }

    #[test]
    fn frame_params_locates_a_cycle_in_the_frame() {
        let p = FrameParams::new(254, 264, 240);
        assert_eq!(p.cycles_per_frame(), 67_056);
        assert_eq!(p.position(0), (0, true));
        assert_eq!(p.position(1), (0, false));
        assert_eq!(p.position(254), (1, true));
        assert_eq!(p.position(253 * 254 + 3), (253, false));
        // Wraps into the next frame.
        assert_eq!(p.position(67_056), (0, true));
        assert_eq!(p.position(67_056 + 254), (1, true));
        assert!(!p.in_vblank(239));
        assert!(p.in_vblank(240));
    }
}
