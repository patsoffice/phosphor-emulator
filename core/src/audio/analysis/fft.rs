//! Radix-2 FFT and windowing.
//!
//! Hand-rolled rather than pulled from a crate, for the same reason the FIR and
//! the resampler are: `phosphor-core` carries no external dependencies, and an
//! iterative Cooley-Tukey is about forty lines. Analysis frames here are a few
//! thousand samples, where the constant factor of a tuned library would not pay
//! for the dependency.

use std::f64::consts::{PI, TAU};

/// Minimal complex number. Local to the transform — callers deal in real
/// slices and magnitude spectra.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Complex {
    pub re: f64,
    pub im: f64,
}

impl Complex {
    fn magnitude(self) -> f64 {
        self.re.hypot(self.im)
    }
}

/// Round up to the next power of two, which is the only length the radix-2
/// transform accepts.
pub fn next_power_of_two(n: usize) -> usize {
    n.max(1).next_power_of_two()
}

/// In-place iterative Cooley-Tukey FFT. `buf.len()` must be a power of two.
///
/// Decimation in time: the bit-reversal permutation up front puts each input
/// where its butterfly needs it, then `len` doubles from 2 to `n`, combining
/// adjacent half-transforms.
pub(crate) fn fft_in_place(buf: &mut [Complex]) {
    let n = buf.len();
    debug_assert!(n.is_power_of_two(), "fft length {n} is not a power of two");
    if n < 2 {
        return;
    }

    // Bit-reversal permutation.
    let mut target = 0usize;
    for pos in 1..n {
        // Increment `target` as if its bits ran the other way, carrying left.
        let mut mask = n >> 1;
        while target & mask != 0 {
            target &= !mask;
            mask >>= 1;
        }
        target |= mask;
        if target > pos {
            buf.swap(pos, target);
        }
    }

    // Butterflies, halves combining upward.
    let mut len = 2;
    while len <= n {
        let step = -TAU / len as f64;
        for chunk in buf.chunks_mut(len) {
            let half = len / 2;
            for k in 0..half {
                let angle = step * k as f64;
                let (sin, cos) = angle.sin_cos();
                let w = Complex { re: cos, im: sin };
                let a = chunk[k];
                let b = chunk[k + half];
                let bw = Complex {
                    re: b.re * w.re - b.im * w.im,
                    im: b.re * w.im + b.im * w.re,
                };
                chunk[k] = Complex {
                    re: a.re + bw.re,
                    im: a.im + bw.im,
                };
                chunk[k + half] = Complex {
                    re: a.re - bw.re,
                    im: a.im - bw.im,
                };
            }
        }
        len <<= 1;
    }
}

/// Magnitude spectrum of a real signal, zero-padded to the next power of two.
///
/// Returns the `n/2 + 1` non-negative-frequency bins, so bin `i` is at
/// `i * sample_rate / n` Hz. Magnitudes are not normalized by `n` — every
/// consumer here is scale-invariant or normalizes itself.
pub fn magnitude_spectrum(samples: &[f64]) -> Vec<f64> {
    let n = next_power_of_two(samples.len());
    let mut buf = vec![Complex::default(); n];
    for (slot, &s) in buf.iter_mut().zip(samples) {
        slot.re = s;
    }
    fft_in_place(&mut buf);
    buf[..n / 2 + 1].iter().map(|c| c.magnitude()).collect()
}

/// Frequency in Hz of spectrum bin `i`, for a transform of length `n`.
pub fn bin_hz(i: usize, n: usize, sample_rate: f64) -> f64 {
    i as f64 * sample_rate / n as f64
}

/// Periodic Hann window of length `n`.
///
/// Periodic (`n` in the denominator) rather than symmetric (`n - 1`) because
/// these windows feed overlapping STFT frames, where the periodic form is the
/// one that sums flat.
pub fn hann_window(n: usize) -> Vec<f64> {
    if n <= 1 {
        return vec![1.0; n];
    }
    (0..n)
        .map(|i| 0.5 - 0.5 * (TAU * i as f64 / n as f64).cos())
        .collect()
}

/// Short-time Fourier transform: magnitude spectra of successive Hann-windowed
/// frames of `window` samples, advanced by `hop`.
///
/// Frames that would run past the end are dropped rather than zero-padded, so
/// every returned frame describes the same amount of real signal.
pub fn stft(samples: &[f64], window: usize, hop: usize) -> Vec<Vec<f64>> {
    assert!(
        window > 0 && hop > 0,
        "stft needs a positive window and hop"
    );
    if samples.len() < window {
        return Vec::new();
    }
    let w = hann_window(window);
    let mut frames = Vec::new();
    let mut start = 0;
    while start + window <= samples.len() {
        let framed: Vec<f64> = samples[start..start + window]
            .iter()
            .zip(&w)
            .map(|(s, k)| s * k)
            .collect();
        frames.push(magnitude_spectrum(&framed));
        start += hop;
    }
    frames
}

/// Autocorrelation of `samples` for lags `0..=max_lag`, normalized so lag 0 is
/// 1.0.
///
/// Computed directly rather than through the FFT: the pitch estimator only asks
/// for a few hundred lags over a few thousand samples, where the direct form is
/// both faster and easier to be sure of.
pub fn autocorrelation(samples: &[f64], max_lag: usize) -> Vec<f64> {
    let energy: f64 = samples.iter().map(|s| s * s).sum();
    if energy <= 0.0 {
        return vec![0.0; max_lag + 1];
    }
    (0..=max_lag)
        .map(|lag| {
            if lag >= samples.len() {
                return 0.0;
            }
            let sum: f64 = samples[..samples.len() - lag]
                .iter()
                .zip(&samples[lag..])
                .map(|(a, b)| a * b)
                .sum();
            sum / energy
        })
        .collect()
}

/// Parabolic interpolation through three samples around a discrete peak at
/// `i`, returning the sub-sample offset from `i` in `[-0.5, 0.5]`.
///
/// A discrete peak quantizes frequency to a bin, which for a 1024-point frame
/// at 44.1 kHz is 43 Hz — coarse enough to matter when comparing pitch. Fitting
/// a parabola through the peak and its neighbours recovers most of that.
pub fn parabolic_offset(prev: f64, peak: f64, next: f64) -> f64 {
    let den = prev - 2.0 * peak + next;
    if den.abs() < f64::EPSILON {
        return 0.0;
    }
    (0.5 * (prev - next) / den).clamp(-0.5, 0.5)
}

/// Convert a linear magnitude to decibels relative to full scale, with a floor
/// so silence produces a very negative number rather than `-inf`.
pub fn db(magnitude: f64) -> f64 {
    const FLOOR: f64 = 1e-10;
    20.0 * magnitude.abs().max(FLOOR).log10()
}

/// Hz per radian conversion used when reporting a normalized frequency.
pub fn radians_to_hz(radians: f64, sample_rate: f64) -> f64 {
    radians * sample_rate / (2.0 * PI)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, rate: f64, n: usize) -> Vec<f64> {
        (0..n)
            .map(|i| (TAU * freq * i as f64 / rate).sin())
            .collect()
    }

    /// The transform of a constant is all energy in bin 0 and nothing else.
    #[test]
    fn dc_lands_entirely_in_bin_zero() {
        let spec = magnitude_spectrum(&[1.0; 64]);
        assert!((spec[0] - 64.0).abs() < 1e-9);
        for (i, m) in spec.iter().enumerate().skip(1) {
            assert!(*m < 1e-9, "bin {i} should be empty, got {m}");
        }
    }

    /// A sine at an exact bin centre puts its energy in that bin alone.
    #[test]
    fn a_bin_centred_sine_lands_in_one_bin() {
        let n = 1024;
        let rate = 1024.0;
        // 64 cycles over 1024 samples = bin 64 exactly.
        let spec = magnitude_spectrum(&sine(64.0, rate, n));
        let peak = spec
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap()
            .0;
        assert_eq!(peak, 64);
        // Neighbours should be near-empty, i.e. no leakage without a window.
        assert!(spec[63] < spec[64] * 1e-6);
        assert!(spec[65] < spec[64] * 1e-6);
    }

    /// Linearity: the transform of a sum is the sum of the transforms, so two
    /// tones produce two peaks at the right places.
    #[test]
    fn two_tones_produce_two_peaks() {
        let n = 1024;
        let rate = 1024.0;
        let mixed: Vec<f64> = sine(64.0, rate, n)
            .iter()
            .zip(sine(200.0, rate, n))
            .map(|(a, b)| a + b)
            .collect();
        let spec = magnitude_spectrum(&mixed);
        assert!(spec[64] > spec[100] * 100.0);
        assert!(spec[200] > spec[100] * 100.0);
    }

    /// Parseval: total energy is preserved between time and frequency domains.
    #[test]
    fn energy_is_preserved() {
        let n = 512;
        let sig = sine(37.0, 512.0, n);
        let time: f64 = sig.iter().map(|s| s * s).sum();

        let mut buf = vec![Complex::default(); n];
        for (slot, &s) in buf.iter_mut().zip(&sig) {
            slot.re = s;
        }
        fft_in_place(&mut buf);
        let freq: f64 = buf.iter().map(|c| c.re * c.re + c.im * c.im).sum::<f64>() / n as f64;
        assert!(
            (time - freq).abs() < 1e-6 * time,
            "time {time} vs freq {freq}"
        );
    }

    #[test]
    fn short_inputs_are_zero_padded_to_a_power_of_two() {
        assert_eq!(magnitude_spectrum(&[1.0; 100]).len(), 128 / 2 + 1);
        assert_eq!(next_power_of_two(1), 1);
        assert_eq!(next_power_of_two(100), 128);
        assert_eq!(next_power_of_two(128), 128);
    }

    /// A periodic Hann window starts at zero and its ends do not duplicate,
    /// which is what makes overlapping frames sum flat.
    #[test]
    fn hann_is_periodic_not_symmetric() {
        let w = hann_window(8);
        assert!(w[0].abs() < 1e-12);
        assert!((w[4] - 1.0).abs() < 1e-12);
        // Symmetric would put a second zero at the last sample; periodic does not.
        assert!(w[7] > 0.0);
        // Two half-overlapped Hann windows sum to unity in the middle.
        assert!((w[2] + w[6] - 1.0).abs() < 1e-12);
    }

    #[test]
    fn stft_frame_count_follows_the_hop() {
        let sig = vec![0.0; 1000];
        assert_eq!(stft(&sig, 256, 128).len(), (1000 - 256) / 128 + 1);
        // Too short for even one frame.
        assert!(stft(&[0.0; 100], 256, 128).is_empty());
    }

    /// Autocorrelation peaks at the signal's own period, which is what makes it
    /// a pitch estimator that does not care about the harmonic structure.
    #[test]
    fn autocorrelation_peaks_at_the_period() {
        let rate = 8000.0;
        let freq = 200.0;
        let sig = sine(freq, rate, 4000);
        let period = (rate / freq) as usize; // 40 samples
        let ac = autocorrelation(&sig, 200);
        assert!((ac[0] - 1.0).abs() < 1e-9);
        // The first strong peak after lag 0 sits at one period.
        let best = (10..100)
            .max_by(|&a, &b| ac[a].partial_cmp(&ac[b]).unwrap())
            .unwrap();
        assert_eq!(best, period);
    }

    #[test]
    fn silence_autocorrelates_to_nothing() {
        assert!(autocorrelation(&[0.0; 100], 10).iter().all(|&v| v == 0.0));
    }

    /// A symmetric peak needs no correction; a lopsided one leans toward the
    /// taller neighbour.
    #[test]
    fn parabolic_offset_leans_toward_the_taller_neighbour() {
        assert!((parabolic_offset(1.0, 2.0, 1.0)).abs() < 1e-12);
        assert!(parabolic_offset(1.0, 2.0, 1.5) > 0.0);
        assert!(parabolic_offset(1.5, 2.0, 1.0) < 0.0);
        assert!(parabolic_offset(0.0, 0.0, 0.0).abs() < 1e-12);
    }

    #[test]
    fn db_has_a_floor_instead_of_negative_infinity() {
        assert!((db(1.0)).abs() < 1e-12);
        assert!((db(0.5) + 6.0206).abs() < 1e-3);
        assert!(db(0.0).is_finite());
        assert!(db(0.0) < -190.0);
    }
}
