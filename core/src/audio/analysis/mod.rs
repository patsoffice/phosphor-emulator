//! Offline audio analysis: the arithmetic behind comparing two captures.
//!
//! This is the shared half of the discrete-sound fidelity tooling
//! (`docs/designs/discrete-sound-fidelity.md`). It lives in `phosphor-core`
//! because both consumers already depend on core and neither should own a
//! second copy: `disasm audiodiff` compares two WAVs, and the registry audio
//! sanity test asserts that no machine emits DC, silence or clipping it did not
//! declare.
//!
//! Nothing here runs during emulation. It is batch code over a finished
//! capture, and it deliberately trades speed for being obviously correct.
//!
//! # What the metrics are for
//!
//! Comparing two emulators' audio is not comparing two waveforms. Independent
//! LFSR seeds, oscillator phase and startup delay mean the samples never line
//! up, so every measurement here is either scale-invariant, phase-invariant, or
//! both. The groups answer different questions:
//!
//! - [`Integrity`] — is this capture *usable*? DC offset, clipping and dead
//!   silence are defects in the recording or the model, not differences to
//!   explain.
//! - [`Level`] — how loud, and how does that change over time? Attack and decay
//!   live here, and a windowed spectrum averages them away.
//! - [`Spectrum`] — what does it sound like? Centroid, flatness and band ratios
//!   separate a filter error from a mix error from a gain error.
//!
//! Band energy ratios are the load-bearing measurement when diffing two
//! captures: they are scale-invariant, so a pure gain error leaves them all at
//! zero and anything large is a filter, mix or source difference.

pub mod fft;
mod level;
mod spectrum;

pub use level::{
    Integrity, Level, dc_offset, decay_time, integrated_energy, onset_index, rms, rms_dbfs,
    rms_envelope,
};
pub use spectrum::{
    BAND_EDGES_HZ, Peak, Spectrum, band_energy_ratios, dominant_peaks, envelope_alignment,
    envelope_distance, fundamental_hz, gain_ratio, harmonic_ratios, spectral_centroid,
    spectral_flatness, spectral_rolloff, stft_distance,
};

/// Convert 16-bit PCM to `f64` in `[-1, 1)`.
///
/// `i16::MIN` maps to exactly -1.0 and `i16::MAX` to just under +1.0, matching
/// the asymmetry of the format rather than hiding it.
pub fn pcm_to_f64(samples: &[i16]) -> Vec<f64> {
    samples
        .iter()
        .map(|&s| s as f64 / -(i16::MIN as f64))
        .collect()
}

/// Remove the mean from a signal, returning the AC component.
///
/// Every spectral measurement wants this: a DC offset is a huge bin-0 magnitude
/// that drags the centroid down and inflates RMS, so it would otherwise
/// masquerade as low-frequency content. The offset itself is worth reporting on
/// its own ([`dc_offset`]), which is why removing it here is a separate step
/// rather than something the metrics do silently.
pub fn remove_dc(samples: &[f64]) -> Vec<f64> {
    let mean = dc_offset(samples);
    samples.iter().map(|s| s - mean).collect()
}

/// Everything measurable about one capture.
///
/// Produced by [`analyze`], which is what both `audiodiff` and the sanity test
/// call. The split into three groups is the reporting structure, not just
/// bookkeeping — see the module docs.
#[derive(Clone, Debug)]
pub struct Analysis {
    pub sample_rate: f64,
    pub samples: usize,
    pub integrity: Integrity,
    pub level: Level,
    pub spectrum: Spectrum,
}

impl Analysis {
    /// Duration of the analyzed capture in seconds.
    pub fn duration_s(&self) -> f64 {
        self.samples as f64 / self.sample_rate
    }
}

/// Measure one capture end to end.
///
/// `samples` is expected in `[-1, 1]`; use [`pcm_to_f64`] for a 16-bit capture.
/// Integrity metrics see the raw signal (a DC offset must stay visible), while
/// level and spectral metrics see the DC-removed copy.
pub fn analyze(samples: &[f64], sample_rate: f64) -> Analysis {
    let integrity = Integrity::measure(samples);
    let ac = remove_dc(samples);
    Analysis {
        sample_rate,
        samples: samples.len(),
        integrity,
        level: Level::measure(&ac, sample_rate),
        spectrum: Spectrum::measure(&ac, sample_rate),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    pub(crate) fn sine(freq: f64, rate: f64, n: usize) -> Vec<f64> {
        (0..n)
            .map(|i| (TAU * freq * i as f64 / rate).sin())
            .collect()
    }

    #[test]
    fn pcm_conversion_spans_the_full_range() {
        let v = pcm_to_f64(&[i16::MIN, 0, i16::MAX]);
        assert_eq!(v[0], -1.0);
        assert_eq!(v[1], 0.0);
        assert!(v[2] > 0.999 && v[2] < 1.0);
    }

    #[test]
    fn remove_dc_centres_a_biased_signal() {
        let biased: Vec<f64> = sine(100.0, 8000.0, 8000).iter().map(|s| s + 0.3).collect();
        assert!((dc_offset(&biased) - 0.3).abs() < 1e-3);
        assert!(dc_offset(&remove_dc(&biased)).abs() < 1e-12);
    }

    /// The whole pipeline on a known signal: a 1 kHz sine is clean, tonal, and
    /// sits at the right level and pitch.
    ///
    /// Amplitude 0.9 rather than full scale on purpose — a sine that touches
    /// 1.0 registers those samples as clipped, which is the metric behaving
    /// correctly but makes for a confusing "clean signal" fixture.
    #[test]
    fn analyze_reads_a_clean_sine_correctly() {
        let s: Vec<f64> = sine(1000.0, 44100.0, 44100)
            .iter()
            .map(|v| v * 0.9)
            .collect();
        let a = analyze(&s, 44100.0);
        assert!((a.duration_s() - 1.0).abs() < 1e-9);
        assert!(a.integrity.dc_offset.abs() < 1e-3);
        assert_eq!(a.integrity.clipped, 0);
        assert!(!a.integrity.is_silent);
        // A 0.9-amplitude sine is -3.01 - 0.92 = -3.93 dBFS RMS.
        assert!(
            (a.level.rms_dbfs + 3.93).abs() < 0.1,
            "{}",
            a.level.rms_dbfs
        );
        assert!((a.spectrum.centroid_hz - 1000.0).abs() < 20.0);
        assert!(a.spectrum.flatness < 0.01, "a sine is not noise");
        assert!((a.spectrum.fundamental_hz - 1000.0).abs() < 5.0);
    }
}
