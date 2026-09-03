//! Two-pole filter sections, for the ones an arcade board builds out of an
//! op-amp and four passives.
//!
//! [`DcBlocker`](super::DcBlocker) is the one-pole case and is common enough to
//! have its own type. This is the two-pole case: a Sallen-Key section, which is
//! what a board reaches for when one pole is not a steep enough skirt — either
//! side of a delay line, ahead of a sampler, or across a speaker.
//!
//! # Why `f0` and `Q` rather than components
//!
//! A transcription reads resistors and capacitors, but two boards with different
//! parts can build the same response, and the same parts in two topologies do
//! not. Deriving `f0` and `Q` at the call site keeps that arithmetic next to the
//! reference designators it came from, where a reader can check it against the
//! drawing, and leaves this type with one job. `discrete::second_order` is the
//! same filter as a graph node for the boards that build a whole circuit that
//! way; this is the one for a board that needs a section in a hand-written path.
//!
//! # The form
//!
//! A direct-form-II transposed biquad, which is the numerically better-behaved
//! arrangement at `f32` and holds two state variables instead of four. The
//! coefficients come from the RBJ audio-EQ cookbook's bilinear transform, with
//! the usual `f0 << rate / 2` caveat: a corner near Nyquist warps, and this
//! clamps rather than pretending otherwise.

/// A two-pole section.
///
/// Construct with [`Self::low_pass`]; the state is two samples of history and
/// the coefficients are fixed at construction.
#[derive(Debug, Clone)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    s1: f32,
    s2: f32,
}

impl Biquad {
    /// A two-pole low-pass at `f0_hz` with quality factor `q`.
    ///
    /// `q = 0.7071` (`1/sqrt(2)`) is the Butterworth case — maximally flat, no
    /// peak at the corner — and is what an equal-resistor Sallen-Key section
    /// gives when the bridging capacitor is twice the shunt one. Boards land on
    /// it by using one capacitor value and doubling it, which is worth
    /// recognising in a transcription.
    pub fn low_pass(f0_hz: f32, q: f32, sample_rate: u32) -> Self {
        let fs = sample_rate.max(1) as f32;
        // Keep the corner inside the band the bilinear transform can represent.
        // A board filter above Nyquist is a transcription error, not something
        // to model, but it must not produce NaN coefficients here. The lower
        // bound is itself clamped: at a degenerate sample rate the usable band
        // is narrower than 1 Hz, and `clamp` panics if its bounds cross.
        let hi = fs * 0.45;
        let f0 = f0_hz.clamp(hi.min(1.0), hi.max(f32::MIN_POSITIVE));
        let q = q.max(0.01);

        let w0 = std::f32::consts::TAU * f0 / fs;
        let (sin_w0, cos_w0) = w0.sin_cos();
        let alpha = sin_w0 / (2.0 * q);

        let b0 = (1.0 - cos_w0) / 2.0;
        let b1 = 1.0 - cos_w0;
        let b2 = b0;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha;

        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            s1: 0.0,
            s2: 0.0,
        }
    }

    /// Filter one sample.
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.s1;
        self.s1 = self.b1 * x - self.a1 * y + self.s2;
        self.s2 = self.b2 * x - self.a2 * y;
        y
    }

    /// Clear the filter's history, for the same reason
    /// [`DcBlocker::reset`](super::DcBlocker::reset) exists.
    pub fn reset(&mut self) {
        self.s1 = 0.0;
        self.s2 = 0.0;
    }
}

/// The two state variables are live state and a save state has to carry them,
/// exactly as for [`DcBlocker`](super::DcBlocker). The coefficients are derived
/// from the corner and the sample rate at construction and are not serialized.
impl crate::core::save_state::Saveable for Biquad {
    fn save_state(&self, w: &mut crate::core::save_state::StateWriter) {
        w.write_f32_le(self.s1);
        w.write_f32_le(self.s2);
    }

    fn load_state(
        &mut self,
        r: &mut crate::core::save_state::StateReader,
    ) -> Result<(), crate::core::save_state::SaveError> {
        self.s1 = r.read_f32_le()?;
        self.s2 = r.read_f32_le()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    const RATE: u32 = 44_100;

    /// Peak amplitude of a settled sine at `freq`, which is the filter's
    /// magnitude response there.
    fn response(f: &mut Biquad, freq: f32) -> f32 {
        let n = RATE as usize;
        let mut peak: f32 = 0.0;
        for i in 0..n {
            let y = f.process((TAU * freq * i as f32 / RATE as f32).sin());
            if i > n / 2 {
                peak = peak.max(y.abs());
            }
        }
        peak
    }

    /// DC and the deep passband come through at unity: a low-pass that costs
    /// level in its own passband is a gain error hiding in a filter.
    #[test]
    fn the_passband_is_unity() {
        let mut f = Biquad::low_pass(3473.0, 0.707, RATE);
        assert!((response(&mut f, 100.0) - 1.0).abs() < 0.01);
    }

    /// The defining property of the corner frequency.
    #[test]
    fn butterworth_is_3db_down_at_the_corner() {
        let mut f = Biquad::low_pass(3473.0, std::f32::consts::FRAC_1_SQRT_2, RATE);
        let db = 20.0 * response(&mut f, 3473.0).log10();
        assert!(
            (db + 3.01).abs() < 0.2,
            "corner was {db} dB, expected -3.01"
        );
    }

    /// Two poles is 12 dB per octave in the stopband, which is the whole reason
    /// a board spends an op-amp on this instead of an RC.
    ///
    /// Measured well below Nyquist. The bilinear transform warps the response
    /// as the frequency approaches it — an octave from 4 to 8 kHz at 44.1 kHz
    /// already reads 13.3 dB rather than 12 — and that is the transform being
    /// correct, not the filter being wrong.
    #[test]
    fn the_stopband_falls_twelve_db_per_octave() {
        let q = std::f32::consts::FRAC_1_SQRT_2;
        let mut f = Biquad::low_pass(200.0, q, RATE);
        let a = 20.0 * response(&mut f, 800.0).log10();
        let mut f = Biquad::low_pass(200.0, q, RATE);
        let b = 20.0 * response(&mut f, 1600.0).log10();
        assert!((a - b - 12.0).abs() < 0.3, "{a} dB to {b} dB is not 12/oct");
    }

    /// Butterworth means no peak: a resonant section would exceed unity just
    /// below the corner and this must not.
    #[test]
    fn butterworth_does_not_peak() {
        for freq in [500.0, 1000.0, 2000.0, 3000.0, 3473.0] {
            let mut f = Biquad::low_pass(3473.0, std::f32::consts::FRAC_1_SQRT_2, RATE);
            assert!(
                response(&mut f, freq) <= 1.005,
                "peaked at {freq} Hz: {}",
                response(&mut Biquad::low_pass(3473.0, 0.707, RATE), freq)
            );
        }
    }

    /// A higher Q does peak, which is what distinguishes the two cases.
    #[test]
    fn a_high_q_peaks_near_the_corner() {
        let mut f = Biquad::low_pass(1000.0, 5.0, RATE);
        assert!(response(&mut f, 1000.0) > 4.0);
    }

    #[test]
    fn silence_stays_silent_and_reset_clears() {
        let mut f = Biquad::low_pass(3473.0, 0.707, RATE);
        for _ in 0..100 {
            f.process(1.0);
        }
        f.reset();
        assert_eq!(f.process(0.0), 0.0);
    }

    /// A degenerate rate or a corner above Nyquist must not produce NaN.
    #[test]
    fn pathological_parameters_stay_finite() {
        for (f0, q, rate) in [(3473.0, 0.707, 0), (1e9, 0.707, RATE), (100.0, 0.0, RATE)] {
            let mut f = Biquad::low_pass(f0, q, rate);
            for _ in 0..1000 {
                assert!(f.process(0.5).is_finite(), "{f0} {q} {rate}");
            }
        }
    }
}
