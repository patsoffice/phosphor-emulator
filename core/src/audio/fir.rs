//! Stage two of the resampler: a windowed-sinc anti-alias filter that
//! decimates a fixed 4:1 down to the output rate.
//!
//! # Why a second stage
//!
//! Stage one box-averages the chip's native clock down to an intermediate rate
//! and is nearly free — one add per emulated cycle. But a length-`N` box filter
//! has magnitude response `|sin(πfN/fs) / (N·sin(πf/fs))|`: its first sidelobe
//! is only about 13 dB down and the sidelobes decay at 6 dB per octave. Taking
//! a box straight to 44.1 kHz passes content at, say, 25 kHz at roughly −5 dB,
//! and that content folds to 19.1 kHz. Square waves and LFSR noise — which is
//! all these chips emit — carry an Nth harmonic that falls off only as 1/N, so
//! there is plenty of energy up there to fold. Because the fold is a reflection
//! rather than a shift, it lands at frequencies unrelated to the fundamental,
//! which is what makes it audible as grit rather than as brightness.
//!
//! Splitting the ratio fixes this. Stage one only has to reach
//! [`DECIMATION`]× the output rate, so the whole audio band sits far inside the
//! box's flat region and nothing in it has aliased yet. Stage two is then a
//! real lowpass with a genuine stopband, and it runs once per *output* sample —
//! the per-emulated-cycle path is untouched.
//!
//! ```text
//! 1.79 MHz ──► box decimate ──► 176.4 kHz ──► this filter ──► 44.1 kHz
//!              1 add/cycle       (4× target)   101 taps/output
//! ```
//!
//! # What is left over
//!
//! Stage one still aliases around multiples of the intermediate rate: input
//! content at `f_int − δ` folds to `δ`, which stage two cannot undo. The box's
//! response is what protects that, and it happens to have the right shape —
//! near the box's null at `f_int` the rejection is deep (about −45 dB for
//! content folding to 1 kHz) and it weakens towards the top of the band (about
//! −19 dB for content folding to 18 kHz). Low-frequency aliases, the audible
//! ones, are the well-protected case.

use crate::core::save_state::SaveError;
use std::sync::LazyLock;

/// Ratio between the intermediate rate and the output rate.
///
/// Four is the balance point. Lower puts the box's null closer to the audio
/// band, where its rejection of the fold-down zone is weaker; higher makes the
/// filter longer for the same transition width, since the transition is a fixed
/// fraction of the *output* rate but the tap count scales with the
/// *intermediate* rate.
pub const DECIMATION: usize = 4;

/// Filter length. Odd, so the filter is symmetric about a whole sample and the
/// group delay is exactly `(TAPS - 1) / 2` intermediate samples — 50 here, or
/// about 0.28 ms at 44.1 kHz.
///
/// Sized from the Kaiser order estimate `N ≈ (A − 8) / (2.285·Δω)`: 80 dB of
/// attenuation across a transition from 17.4 kHz to 26.7 kHz (referred to a
/// 44.1 kHz output) needs about 100.
pub const TAPS: usize = 101;

/// Kaiser shape parameter, from `β = 0.1102·(A − 8.7)` for a target stopband
/// attenuation `A` of 80 dB.
const KAISER_BETA: f64 = 7.86;

/// Coefficients of the decimation lowpass, normalised to unit DC gain.
///
/// Built once on first use rather than written out as a literal table, so the
/// design — cutoff, window, normalisation — stays readable and adjustable.
static COEFFICIENTS: LazyLock<[f32; TAPS]> = LazyLock::new(design);

/// The filter's taps.
#[inline]
pub fn coefficients() -> &'static [f32; TAPS] {
    &COEFFICIENTS
}

/// Modified Bessel function of the first kind, order zero, by its power series
/// `Σ ((x/2)^k / k!)²`. Converges in a handful of terms for the β we use.
fn bessel_i0(x: f64) -> f64 {
    let half = x / 2.0;
    let mut sum = 1.0;
    let mut term = 1.0;
    for k in 1..64 {
        let ratio = half / k as f64;
        term *= ratio * ratio;
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

/// Design the windowed-sinc lowpass.
///
/// The cutoff sits at the output Nyquist — normalised to the intermediate rate
/// that is `1 / (2·DECIMATION)` — which is exactly where an ideal decimator
/// cuts. The Kaiser window trades transition width against stopband depth; at
/// 101 taps and β = 7.86 the result is flat to about 17.4 kHz and 80 dB down
/// from about 26.7 kHz (referred to a 44.1 kHz output), so whatever folds is
/// confined above 17.4 kHz.
fn design() -> [f32; TAPS] {
    let cutoff = 1.0 / (2.0 * DECIMATION as f64); // cycles per intermediate sample
    let center = (TAPS - 1) as f64 / 2.0;
    let i0_beta = bessel_i0(KAISER_BETA);

    let mut taps = [0.0f64; TAPS];
    let mut sum = 0.0;
    for (n, tap) in taps.iter_mut().enumerate() {
        let t = n as f64 - center;

        // Ideal lowpass impulse response, 2·fc·sinc(2·fc·t).
        let x = 2.0 * cutoff * t;
        let sinc = if x == 0.0 {
            1.0
        } else {
            (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
        };

        // Kaiser window.
        let r = t / center;
        let w = bessel_i0(KAISER_BETA * (1.0 - r * r).max(0.0).sqrt()) / i0_beta;

        *tap = 2.0 * cutoff * sinc * w;
        sum += *tap;
    }

    // Normalise to unit DC gain so a constant input passes through unchanged.
    std::array::from_fn(|n| (taps[n] / sum) as f32)
}

/// Width of the dot product's accumulator bank.
///
/// A single running sum makes each multiply-add wait on the previous one, and
/// float addition is not associative so the compiler may not split the chain
/// itself. Summing into four independent lanes and combining at the end breaks
/// the dependency and lets the loop vectorise — worth about 3× on the filter,
/// which is most of its cost. The lane count is fixed, so the summation order
/// is fixed too and results stay bit-for-bit reproducible.
const LANES: usize = 4;

/// Inner product of the taps with the delay line.
///
/// Folding the filter about its centre — linear phase means
/// `h[j] == h[TAPS-1-j]` — would halve the multiplies, but the reversed access
/// on one half costs more in lost vectorisation than the saved multiplies are
/// worth. Measured, it was within noise of this straight loop, so the simpler
/// one stays.
#[inline]
fn dot(taps: &[f32; TAPS], history: &[f32; TAPS]) -> f32 {
    let mut acc = [0.0f32; LANES];
    let mut i = 0;
    while i + LANES <= TAPS {
        for (lane, a) in acc.iter_mut().enumerate() {
            *a += taps[i + lane] * history[i + lane];
        }
        i += LANES;
    }
    let mut sum = acc.iter().sum::<f32>();
    while i < TAPS {
        sum += taps[i] * history[i];
        i += 1;
    }
    sum
}

/// The stage-two filter's running state: a delay line of intermediate-rate
/// samples plus the partial group waiting to complete an output.
#[derive(Debug, Clone)]
pub struct DecimatingFir {
    /// Last [`TAPS`] intermediate samples, oldest first.
    history: [f32; TAPS],
    /// Intermediate samples accepted since the last output.
    pending: [f32; DECIMATION],
    pending_len: usize,
}

impl Default for DecimatingFir {
    fn default() -> Self {
        Self::new()
    }
}

impl DecimatingFir {
    /// A filter with a zeroed delay line.
    pub fn new() -> Self {
        Self {
            history: [0.0; TAPS],
            pending: [0.0; DECIMATION],
            pending_len: 0,
        }
    }

    /// Accept one intermediate-rate sample, returning an output sample on every
    /// [`DECIMATION`]th call.
    #[inline]
    pub fn push(&mut self, sample: f32) -> Option<f32> {
        self.pending[self.pending_len] = sample;
        self.pending_len += 1;
        if self.pending_len < DECIMATION {
            return None;
        }
        self.pending_len = 0;

        // Shift the delay line by one output period and append the group. One
        // memmove per output sample, which keeps the dot product below over a
        // contiguous slice the compiler can vectorise.
        self.history.copy_within(DECIMATION.., 0);
        self.history[TAPS - DECIMATION..].copy_from_slice(&self.pending);

        Some(dot(coefficients(), &self.history))
    }

    /// Zero the delay line.
    pub fn reset(&mut self) {
        self.history = [0.0; TAPS];
        self.pending = [0.0; DECIMATION];
        self.pending_len = 0;
    }

    /// The delay line, oldest first. For save state.
    pub fn history(&self) -> &[f32; TAPS] {
        &self.history
    }

    /// The intermediate samples accepted since the last output. For save state.
    pub fn pending(&self) -> &[f32] {
        &self.pending[..self.pending_len]
    }

    /// Restore a saved delay line. `pending` must be shorter than
    /// [`DECIMATION`]; a full group would already have produced an output.
    pub fn restore(&mut self, history: [f32; TAPS], pending: &[f32]) -> Result<(), SaveError> {
        if pending.len() >= DECIMATION {
            return Err(SaveError::InvalidFormat(format!(
                "resampler filter has {} pending samples, expected under {DECIMATION}",
                pending.len()
            )));
        }
        self.history = history;
        self.pending = [0.0; DECIMATION];
        self.pending[..pending.len()].copy_from_slice(pending);
        self.pending_len = pending.len();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Magnitude response at `f`, in cycles per intermediate sample.
    fn response(f: f64) -> f64 {
        let taps = coefficients();
        let center = (TAPS - 1) as f64 / 2.0;
        let (mut re, mut im) = (0.0, 0.0);
        for (n, &h) in taps.iter().enumerate() {
            let phase = -2.0 * std::f64::consts::PI * f * (n as f64 - center);
            re += h as f64 * phase.cos();
            im += h as f64 * phase.sin();
        }
        (re * re + im * im).sqrt()
    }

    #[test]
    fn dc_gain_is_unity() {
        let sum: f64 = coefficients().iter().map(|&h| h as f64).sum();
        assert!((sum - 1.0).abs() < 1e-6, "DC gain {sum}");
    }

    #[test]
    fn taps_are_symmetric() {
        let taps = coefficients();
        for n in 0..TAPS / 2 {
            assert!(
                (taps[n] - taps[TAPS - 1 - n]).abs() < 1e-9,
                "tap {n} is not mirrored"
            );
        }
    }

    #[test]
    fn passband_is_flat_to_seventeen_kilohertz() {
        // Normalised to the intermediate rate, the output rate is 1/DECIMATION,
        // so 17.4 kHz of a 44.1 kHz output is 17400/44100/4.
        for hz in [0.0, 1_000.0, 5_000.0, 10_000.0, 17_400.0] {
            let g = response(hz / 44_100.0 / DECIMATION as f64);
            let db = 20.0 * g.log10();
            assert!(db > -1.0, "{hz} Hz is {db:.2} dB, expected flat");
        }
    }

    #[test]
    fn stopband_is_eighty_decibels_down() {
        // Anything at or above 26.7 kHz would fold below 17.4 kHz on decimation.
        for hz in [26_700.0, 30_000.0, 44_100.0, 66_000.0, 88_200.0] {
            let g = response(hz / 44_100.0 / DECIMATION as f64);
            let db = 20.0 * g.log10();
            assert!(db < -78.0, "{hz} Hz is only {db:.2} dB down");
        }
    }

    #[test]
    fn constant_input_settles_to_that_constant() {
        let mut fir = DecimatingFir::new();
        let mut last = 0.0;
        for _ in 0..1000 {
            if let Some(y) = fir.push(0.75) {
                last = y;
            }
        }
        assert!((last - 0.75).abs() < 1e-5, "settled to {last}");
    }

    #[test]
    fn output_arrives_once_per_decimation_group() {
        let mut fir = DecimatingFir::new();
        let outputs = (0..40).filter(|_| fir.push(1.0).is_some()).count();
        assert_eq!(outputs, 40 / DECIMATION);
    }

    #[test]
    fn restore_round_trips_and_rejects_a_full_group() {
        let mut fir = DecimatingFir::new();
        for i in 0..6 {
            fir.push(i as f32);
        }
        let history = *fir.history();
        let pending = fir.pending().to_vec();

        let mut other = DecimatingFir::new();
        other.restore(history, &pending).unwrap();
        assert_eq!(other.history(), fir.history());
        assert_eq!(other.pending(), fir.pending());

        assert!(
            other.restore(history, &[0.0; DECIMATION]).is_err(),
            "a full group should have produced an output already"
        );
    }
}
