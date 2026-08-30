//! Coupling capacitor: remove the DC pedestal from a unipolar source.
//!
//! Chips like POKEY drive a resistor network with a *unipolar* signal — every
//! channel contributes zero or its volume, so the output swings between 0 and
//! full scale and never goes negative. Its average is whatever the channels
//! happen to be doing, and at rest it is zero, not half scale.
//!
//! On the board that output reaches the amplifier through a coupling capacitor,
//! which passes the audio and blocks the pedestal. Without something playing
//! that role in the emulator, a unipolar signal has to be centred by hand, and
//! the obvious guess — subtract half scale — is wrong in the worst possible
//! way: it maps *silence* to *negative full scale*, so a machine that should be
//! quiet instead pins its output at the rail.
//!
//! That is not hypothetical. Missile Command, Quantum and Food Fight all
//! shipped with a hand-rolled `(s - 0.5) * 2` and all three emitted a constant
//! -32767 when their POKEYs were idle.
//!
//! This is the capacitor: a one-pole high-pass that tracks and subtracts
//! whatever the DC level actually is.

/// One-pole DC blocker — the emulated coupling capacitor.
///
/// `y[n] = x[n] − x[n−1] + R·y[n−1]`, the standard difference form, with `R`
/// set from the corner frequency. It settles to zero output for any constant
/// input, whatever that constant is, so it needs no assumption about where the
/// source's midpoint sits.
#[derive(Debug, Clone)]
pub struct DcBlocker {
    prev_in: f32,
    prev_out: f32,
    r: f32,
}

impl DcBlocker {
    /// Corner frequency in Hz.
    ///
    /// Low enough to pass the whole audible band untouched — a 20 Hz corner is
    /// already below what an arcade cabinet speaker reproduces — and high
    /// enough to settle in a few tens of milliseconds rather than lingering as
    /// an audible thump after a loud passage. Real coupling capacitors on these
    /// boards sit in the same region.
    pub const DEFAULT_CUTOFF_HZ: f32 = 10.0;

    /// A blocker at [`Self::DEFAULT_CUTOFF_HZ`] for the given sample rate.
    pub fn new(sample_rate: u32) -> Self {
        Self::with_cutoff(Self::DEFAULT_CUTOFF_HZ, sample_rate)
    }

    /// A blocker at an explicit corner frequency.
    pub fn with_cutoff(cutoff_hz: f32, sample_rate: u32) -> Self {
        let fs = sample_rate.max(1) as f32;
        // R = 1 − 2π·fc/fs, the one-pole pole position. Clamped below 1.0 so a
        // pathological rate cannot make the filter unstable or a no-op.
        let r = (1.0 - std::f32::consts::TAU * cutoff_hz / fs).clamp(0.0, 0.999_99);
        Self {
            prev_in: 0.0,
            prev_out: 0.0,
            r,
        }
    }

    /// Filter one sample.
    pub fn process(&mut self, x: f32) -> f32 {
        let y = x - self.prev_in + self.r * self.prev_out;
        self.prev_in = x;
        self.prev_out = y;
        y
    }

    /// Filter a buffer in place.
    pub fn process_slice(&mut self, samples: &mut [f32]) {
        for s in samples.iter_mut() {
            *s = self.process(*s);
        }
    }

    /// Clear the filter's history.
    ///
    /// Worth doing on machine reset so a loud passage before the reset cannot
    /// bleed a settling transient into the first frames after it.
    pub fn reset(&mut self) {
        self.prev_in = 0.0;
        self.prev_out = 0.0;
    }
}

/// The filter's two-sample history is live state, so a save state has to carry
/// it: a machine restored mid-passage would otherwise re-settle from zero and
/// disagree with the run it was restored from. `r` is derived from the sample
/// rate at construction and is not serialized — reconstructing the machine
/// establishes it, exactly as it does for the resamplers.
impl crate::core::save_state::Saveable for DcBlocker {
    fn save_state(&self, w: &mut crate::core::save_state::StateWriter) {
        w.write_f32_le(self.prev_in);
        w.write_f32_le(self.prev_out);
    }

    fn load_state(
        &mut self,
        r: &mut crate::core::save_state::StateReader,
    ) -> Result<(), crate::core::save_state::SaveError> {
        self.prev_in = r.read_f32_le()?;
        self.prev_out = r.read_f32_le()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    const RATE: u32 = 44_100;

    /// The property the whole type exists for: a constant input — silence from
    /// a unipolar source, at whatever level — settles to zero, rather than
    /// being mapped to a rail.
    #[test]
    fn any_constant_settles_to_zero() {
        for level in [0.0, 0.25, 0.5, 1.0] {
            let mut b = DcBlocker::new(RATE);
            let mut last = 0.0;
            for _ in 0..RATE {
                last = b.process(level);
            }
            assert!(
                last.abs() < 1e-3,
                "constant {level} settled to {last}, not zero"
            );
        }
    }

    /// Silence from an idle POKEY is 0.0, and it must stay 0.0. This is the
    /// exact case that made three machines emit -32767 forever.
    #[test]
    fn idle_unipolar_silence_stays_silent() {
        let mut b = DcBlocker::new(RATE);
        for _ in 0..RATE {
            let y = b.process(0.0);
            assert_eq!(y, 0.0, "silence must produce silence, got {y}");
        }
    }

    /// Audio-band content passes essentially untouched — the filter must not
    /// cost level in the range the machine actually uses.
    #[test]
    fn audio_band_passes_with_full_amplitude() {
        for freq in [100.0, 440.0, 1000.0, 8000.0] {
            let mut b = DcBlocker::new(RATE);
            let mut peak: f32 = 0.0;
            // Skip the settling transient, then measure.
            for i in 0..RATE {
                let x = (TAU * freq * i as f32 / RATE as f32).sin();
                let y = b.process(x);
                if i > RATE / 2 {
                    peak = peak.max(y.abs());
                }
            }
            assert!(
                peak > 0.99,
                "{freq} Hz lost level: peak {peak}, expected ~1.0"
            );
        }
    }

    /// A tone riding on a pedestal comes out centred on zero, keeping its
    /// amplitude. This is the real POKEY case: unipolar audio around a DC
    /// level that depends on what the channels are doing.
    #[test]
    fn a_tone_on_a_pedestal_is_centred_without_losing_amplitude() {
        let mut b = DcBlocker::new(RATE);
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for i in 0..RATE {
            // 0.5 ± 0.5 — a full-scale unipolar tone.
            let x = 0.5 + 0.5 * (TAU * 440.0 * i as f32 / RATE as f32).sin();
            let y = b.process(x);
            if i > RATE / 2 {
                lo = lo.min(y);
                hi = hi.max(y);
            }
        }
        assert!((hi + lo).abs() < 0.02, "not centred: swings {lo}..{hi}");
        assert!(
            (hi - lo - 1.0).abs() < 0.02,
            "amplitude changed: {lo}..{hi}"
        );
    }

    /// Below the corner the filter attenuates, which is the point — that is
    /// where a drifting pedestal lives.
    #[test]
    fn sub_audible_drift_is_attenuated() {
        let mut b = DcBlocker::with_cutoff(10.0, RATE);
        let mut peak: f32 = 0.0;
        for i in 0..RATE * 2 {
            let x = (TAU * 0.5 * i as f32 / RATE as f32).sin();
            let y = b.process(x);
            if i > RATE {
                peak = peak.max(y.abs());
            }
        }
        assert!(peak < 0.2, "0.5 Hz drift passed at {peak}");
    }

    #[test]
    fn reset_clears_history() {
        let mut b = DcBlocker::new(RATE);
        for _ in 0..100 {
            b.process(1.0);
        }
        b.reset();
        assert_eq!(b.process(0.0), 0.0);
    }

    /// A degenerate rate must not produce a NaN or an unstable filter.
    #[test]
    fn a_pathological_rate_stays_stable() {
        let mut b = DcBlocker::new(0);
        for _ in 0..1000 {
            let y = b.process(1.0);
            assert!(y.is_finite());
        }
    }

    #[test]
    fn process_slice_matches_per_sample() {
        let input: Vec<f32> = (0..256)
            .map(|i| 0.5 + 0.4 * (i as f32 / 8.0).sin())
            .collect();
        let mut a = DcBlocker::new(RATE);
        let expected: Vec<f32> = input.iter().map(|&x| a.process(x)).collect();

        let mut b = DcBlocker::new(RATE);
        let mut actual = input.clone();
        b.process_slice(&mut actual);
        assert_eq!(expected, actual);
    }
}
