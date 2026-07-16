//! Hang / idle-loop detection by PC sampling.
//!
//! A hung CPU sits in a tight loop, so its program counter stops advancing
//! past a small window frame after frame. [`HangDetector`] samples each CPU's
//! PC once per frame and reports when it has stayed within a small address
//! window for many consecutive frames — distinguishing a real hang from a
//! legitimate multi-frame wait (EAROM/self-test) via a frame threshold.
//!
//! This promotes the throwaway detector used to crack the Dig Dug boot hang
//! (`docs/debugging-digdug-hang.md`) into a reusable core util, so the
//! headless `disasm trace --hang` and the frontend overlay can share one
//! implementation. It is pure PC sampling — no board instrumentation.
//!
//! The defaults ([`HangDetector::new`]) match that investigation: an 8-byte
//! window (a tight `DJNZ`-style loop spans a few bytes) and a 120-frame
//! threshold (~2 s at 60 Hz, long enough to clear legitimate waits).

/// Per-CPU rolling PC window plus its in-window frame count.
#[derive(Clone, Copy, Debug, Default)]
struct CpuWindow {
    /// Lowest PC seen in the current in-window run.
    lo: u32,
    /// Highest PC seen in the current in-window run.
    hi: u32,
    /// Consecutive frames the PC has stayed within `window` bytes.
    frames: u32,
    /// True once a report has fired for the current window (report once, not
    /// every frame past the threshold).
    reported: bool,
    /// True once this CPU has been observed at least once.
    active: bool,
}

/// A detected hang: a CPU stuck in a small PC window for many frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HangReport {
    /// Index of the stuck CPU.
    pub cpu_index: usize,
    /// PC sampled on the frame the threshold was crossed.
    pub pc: u32,
    /// Lowest PC of the stuck window.
    pub window_lo: u32,
    /// Highest PC of the stuck window.
    pub window_hi: u32,
    /// Consecutive in-window frames at the report.
    pub frames_stuck: u32,
}

/// Detects a CPU spinning in a tight loop by watching its PC stay within a
/// small window across many frames.
///
/// Feed it each CPU's PC once per frame via [`observe`](Self::observe); it
/// returns a [`HangReport`] the first frame a CPU's PC has been in-window for
/// `threshold_frames`, then stays quiet until the PC leaves the window.
#[derive(Clone, Debug)]
pub struct HangDetector {
    /// Largest PC span (`hi - lo`, bytes) still treated as "the same loop".
    window: u32,
    /// Consecutive in-window frames required before reporting.
    threshold_frames: u32,
    /// Per-CPU state, indexed by `cpu_index` (grows on demand).
    cpus: Vec<CpuWindow>,
}

impl HangDetector {
    /// A detector with the Dig Dug defaults: 8-byte window, 120-frame threshold.
    pub fn new() -> Self {
        Self::with_params(8, 120)
    }

    /// A detector with an explicit PC `window` (bytes) and frame `threshold`.
    ///
    /// `threshold_frames` is clamped to at least 1 (a threshold of 0 would
    /// report before any sampling has happened).
    pub fn with_params(window: u32, threshold_frames: u32) -> Self {
        Self {
            window,
            threshold_frames: threshold_frames.max(1),
            cpus: Vec::new(),
        }
    }

    /// Sample `cpu_index`'s program counter for this frame.
    ///
    /// Returns `Some(HangReport)` on the first frame the CPU has stayed within
    /// the PC window for `threshold_frames` consecutive frames; `None`
    /// otherwise (including every frame after the first report, until the PC
    /// leaves the window). Call once per frame per CPU.
    pub fn observe(&mut self, cpu_index: usize, pc: u32) -> Option<HangReport> {
        if cpu_index >= self.cpus.len() {
            self.cpus.resize(cpu_index + 1, CpuWindow::default());
        }
        let w = &mut self.cpus[cpu_index];

        if !w.active {
            // First sample for this CPU: open a fresh window.
            *w = CpuWindow {
                lo: pc,
                hi: pc,
                frames: 1,
                reported: false,
                active: true,
            };
        } else {
            let new_lo = w.lo.min(pc);
            let new_hi = w.hi.max(pc);
            if new_hi - new_lo <= self.window {
                // Still the same loop: widen the window, count the frame.
                w.lo = new_lo;
                w.hi = new_hi;
                w.frames += 1;
            } else {
                // PC jumped clear of the window: the CPU is making progress.
                *w = CpuWindow {
                    lo: pc,
                    hi: pc,
                    frames: 1,
                    reported: false,
                    active: true,
                };
            }
        }

        if w.frames >= self.threshold_frames && !w.reported {
            w.reported = true;
            Some(HangReport {
                cpu_index,
                pc,
                window_lo: w.lo,
                window_hi: w.hi,
                frames_stuck: w.frames,
            })
        } else {
            None
        }
    }

    /// Forget all per-CPU history (e.g. after a machine reset).
    pub fn reset(&mut self) {
        self.cpus.clear();
    }
}

impl Default for HangDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_after_threshold_in_window() {
        let mut d = HangDetector::with_params(8, 3);
        assert_eq!(d.observe(0, 0x1BCC), None); // frame 1
        assert_eq!(d.observe(0, 0x1BCC), None); // frame 2
        // frame 3 crosses the threshold.
        let r = d.observe(0, 0x1BCC).expect("hang report");
        assert_eq!(r.cpu_index, 0);
        assert_eq!(r.pc, 0x1BCC);
        assert_eq!(r.frames_stuck, 3);
        assert_eq!((r.window_lo, r.window_hi), (0x1BCC, 0x1BCC));
    }

    #[test]
    fn tight_loop_within_window_counts_as_stuck() {
        // A DJNZ loop bounces across a few bytes; span 8 stays in-window.
        let mut d = HangDetector::with_params(8, 3);
        assert_eq!(d.observe(0, 0x1BC8), None);
        assert_eq!(d.observe(0, 0x1BD0), None); // span 8, still one window
        let r = d.observe(0, 0x1BCC).expect("hang report");
        assert_eq!((r.window_lo, r.window_hi), (0x1BC8, 0x1BD0));
        assert_eq!(r.frames_stuck, 3);
    }

    #[test]
    fn pc_jump_outside_window_resets_the_count() {
        let mut d = HangDetector::with_params(8, 3);
        d.observe(0, 0x1BCC);
        d.observe(0, 0x1BCC);
        // A far jump: the CPU is making progress, so the count restarts.
        assert_eq!(d.observe(0, 0x4000), None);
        assert_eq!(d.observe(0, 0x4000), None); // count is 2 now, not 4
        // Only after threshold more in-window frames does it report.
        assert!(d.observe(0, 0x4000).is_some());
    }

    #[test]
    fn no_report_for_sub_threshold_wait() {
        // A legitimate wait shorter than the threshold must not report.
        let mut d = HangDetector::with_params(8, 120);
        for _ in 0..119 {
            assert_eq!(d.observe(0, 0x2000), None);
        }
    }

    #[test]
    fn reports_once_then_stays_quiet_until_window_leaves() {
        let mut d = HangDetector::with_params(8, 2);
        assert_eq!(d.observe(0, 0x100), None);
        assert!(d.observe(0, 0x100).is_some()); // first report
        // Continuing in-window does not re-report.
        assert_eq!(d.observe(0, 0x100), None);
        assert_eq!(d.observe(0, 0x100), None);
        // Leaving the window and getting stuck again reports afresh.
        d.observe(0, 0x9000); // reset window
        assert!(d.observe(0, 0x9000).is_some());
    }

    #[test]
    fn cpus_are_independent() {
        let mut d = HangDetector::with_params(8, 2);
        // CPU 0 spins; CPU 1 makes progress each frame.
        assert_eq!(d.observe(0, 0x100), None);
        assert_eq!(d.observe(1, 0x200), None);
        let r = d.observe(0, 0x100).expect("cpu0 hangs");
        assert_eq!(r.cpu_index, 0);
        // CPU 1 moved far; it must not report.
        assert_eq!(d.observe(1, 0x9999), None);
    }

    #[test]
    fn reset_clears_history() {
        let mut d = HangDetector::with_params(8, 2);
        d.observe(0, 0x100);
        d.reset();
        // After reset the next sample is a fresh first observation.
        assert_eq!(d.observe(0, 0x100), None);
        assert!(d.observe(0, 0x100).is_some());
    }

    #[test]
    fn zero_threshold_is_clamped_to_one() {
        let mut d = HangDetector::with_params(8, 0);
        // Clamped to 1: the very first sample reports.
        assert!(d.observe(0, 0x100).is_some());
    }
}
