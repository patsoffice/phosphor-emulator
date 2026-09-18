//! The beam's drive, shared by every board that draws one row at a time.
//!
//! Fourteen boards had written this out by hand, character-identical apart from
//! the CPU complement. The bodies are short, which is exactly why copying them
//! was easy and why the copies drifted out of anyone's attention: the drive is
//! where the **one-line delay** lives, and that is a fidelity property rather
//! than a loop.
//!
//! # What the drive has to get right
//!
//! A row is composited at the *start* of its scanline, out of the video state as
//! it stands at that moment. So a write during scanline `N` is first visible on
//! row `N + 1`, and a board that drew at the end of a row instead would show
//! every mid-frame write one line early. Sprite lists are walked one line ahead
//! of the row they appear on, which is a second delay on top of this one; see
//! `docs/schematics/sprite-list-scan.md` before adding either twice.
//!
//! # Why there are two paths through a cycle
//!
//! [`ScanlineDriven::tick`] tests the frame position on every cycle, because the
//! debugger single-steps and still has to cross scanline boundaries.
//! [`ScanlineDriven::run_scanlines`] hoists that test into an outer loop and runs
//! a whole row between boundaries, which is the path a frame takes. They have to
//! agree, and the reason they are both here rather than on fourteen boards is
//! that one implementation is one place to make them agree.
//!
//! # The lead / whole / trail shape
//!
//! [`ScanlineDriven::run_frame`] runs up to the next scanline boundary one cycle
//! at a time, then whole rows, then whatever is left. The partial ends only
//! arise once the debugger has stepped the clock off-phase; in normal running
//! the lead and trail are both zero and the whole frame goes through the fast
//! path. Getting that wrong is not a crash, it is a board that renders correctly
//! until someone single-steps it.
//!
//! # What this does not cover
//!
//! The loop only. What *surrounds* it stays on the machine, because it differs:
//! `foodf` does its watchdog and audio mixing around the frame, and the
//! Namco Galaga family re-forms its CPU/bus split per scanline. A board whose
//! polygon or vector layer is not raster-driven does not implement this at all.

use phosphor_core::core::machine::TimingConfig;

/// A board whose picture is driven by the beam, one row at a time.
///
/// Implemented on a short-lived driver that holds the CPU complement and the
/// board as two disjoint borrows, so a cycle still dispatches at a concrete type
/// and the split is formed once per frame rather than once per cycle. See
/// `MrdoDrive` in `mrdo.rs` for the smallest example, and note that the free
/// `run_frame` / `run_scanlines` / `tick` functions each board exposes are thin
/// wrappers that build one of these: the call sites did not have to change and
/// the debugger's entry points are unmoved.
pub(crate) trait ScanlineDriven {
    /// The board's own timing, for the scanline and frame lengths.
    const TIMING: TimingConfig;

    /// The board's free-running cycle counter.
    ///
    /// Takes `&mut self` rather than `&self` because the boards that hand their
    /// CPUs a per-game *bus view* reach the board through a `board(&mut self)`
    /// accessor, and there is no immutable counterpart. The driver holds a
    /// mutable borrow for its whole (very short) life in any case, so nothing is
    /// given up by saying so.
    fn clock(&mut self) -> u64;

    /// The board's scanline-boundary work: composite the row about to be
    /// scanned, and whatever else the board hangs off a line boundary.
    fn begin_scanline(&mut self, scanline: u64);

    /// One CPU cycle, with no frame-position test in it.
    fn step_cycle(&mut self);

    /// One CPU cycle, testing the frame position first.
    ///
    /// The debugger's path. A whole frame goes through [`Self::run_frame`],
    /// which hoists the test out.
    #[inline]
    fn tick(&mut self) {
        let frame_cycle = self.clock() % Self::TIMING.cycles_per_frame();
        if frame_cycle.is_multiple_of(Self::TIMING.cycles_per_scanline) {
            self.begin_scanline(frame_cycle / Self::TIMING.cycles_per_scanline);
        }
        self.step_cycle();
    }

    /// Run `cycles` CPU cycles, scanline-outer and cycle-inner.
    ///
    /// The caller must start on a scanline boundary and pass a multiple of
    /// `cycles_per_scanline`; off-boundary stepping goes through [`Self::tick`].
    #[inline]
    fn run_scanlines(&mut self, cycles: u64) {
        let per_line = Self::TIMING.cycles_per_scanline;
        debug_assert!(
            self.clock().is_multiple_of(per_line) && cycles.is_multiple_of(per_line),
            "run_scanlines must start on a scanline boundary and run whole scanlines"
        );
        for _ in 0..cycles / per_line {
            let scanline = self.clock() % Self::TIMING.cycles_per_frame() / per_line;
            self.begin_scanline(scanline);
            for _ in 0..per_line {
                self.step_cycle();
            }
        }
    }

    /// Run one frame's worth of CPU cycles.
    #[inline]
    fn run_frame(&mut self) {
        let per_line = Self::TIMING.cycles_per_scanline;
        let mut remaining = Self::TIMING.cycles_per_frame();

        let lead = ((per_line - self.clock() % per_line) % per_line).min(remaining);
        for _ in 0..lead {
            self.tick();
        }
        remaining -= lead;

        let whole = remaining - remaining % per_line;
        self.run_scanlines(whole);
        remaining -= whole;

        for _ in 0..remaining {
            self.tick();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A board that records what the drive did to it and nothing else. Short
    /// enough to reason about: four cycles to a line, three lines to a frame.
    #[derive(Default)]
    struct Fake {
        clock: u64,
        lines: Vec<u64>,
        cycles: u64,
    }

    const FAKE_TIMING: TimingConfig = TimingConfig {
        cpu_clock_hz: 12,
        cycles_per_scanline: 4,
        total_scanlines: 3,
        display_width: 1,
        display_height: 1,
        display_aspect: None,
    };

    impl ScanlineDriven for Fake {
        const TIMING: TimingConfig = FAKE_TIMING;

        fn clock(&mut self) -> u64 {
            self.clock
        }

        fn begin_scanline(&mut self, scanline: u64) {
            self.lines.push(scanline);
        }

        fn step_cycle(&mut self) {
            self.clock += 1;
            self.cycles += 1;
        }
    }

    /// Every line of the frame gets its boundary, once, in order.
    #[test]
    fn a_frame_begins_every_scanline_exactly_once() {
        let mut f = Fake::default();
        f.run_frame();
        assert_eq!(f.lines, vec![0, 1, 2]);
        assert_eq!(f.cycles, FAKE_TIMING.cycles_per_frame());
    }

    /// The debugger's per-cycle path and the scanline-outer path have to agree,
    /// which is the property that used to be asserted on fourteen boards
    /// separately and is now asserted once. A frame of `tick` must produce the
    /// same boundaries, in the same order, as one `run_frame`.
    #[test]
    fn the_debuggers_path_and_the_fast_path_agree() {
        let mut stepped = Fake::default();
        for _ in 0..FAKE_TIMING.cycles_per_frame() {
            stepped.tick();
        }

        let mut whole = Fake::default();
        whole.run_frame();

        assert_eq!(stepped.lines, whole.lines);
        assert_eq!(stepped.cycles, whole.cycles);
    }

    /// The lead / whole / trail shape exists for the case where the debugger has
    /// left the clock off-phase. The frame still runs exactly one frame's cycles
    /// and still crosses every boundary it should, just not starting at line 0.
    #[test]
    fn an_off_phase_clock_still_crosses_every_boundary() {
        let mut f = Fake {
            // Two cycles into line 1: a partial lead and a partial trail.
            clock: 6,
            ..Fake::default()
        };
        f.run_frame();

        assert_eq!(f.cycles, FAKE_TIMING.cycles_per_frame());
        // From mid-line-1 a frame crosses 2, then wraps through 0 and 1.
        assert_eq!(f.lines, vec![2, 0, 1]);
    }

    /// A frame's worth of cycles from a boundary leaves the clock on a boundary,
    /// so the next frame takes the fast path with no lead at all.
    #[test]
    fn a_frame_from_a_boundary_lands_on_one() {
        let mut f = Fake::default();
        f.run_frame();
        assert!(f.clock.is_multiple_of(FAKE_TIMING.cycles_per_scanline));

        f.lines.clear();
        f.run_frame();
        assert_eq!(f.lines, vec![0, 1, 2], "the second frame repeats the first");
    }
}
