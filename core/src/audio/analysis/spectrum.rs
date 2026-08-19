//! Frequency and timbre: what a capture sounds like, independent of when.
//!
//! Every measurement here is phase-blind, which is not a simplification but a
//! requirement. Two emulators running the same circuit never share LFSR seed or
//! oscillator phase, so any measurement that cared about phase would be
//! measuring seed coincidence.

use super::fft::{bin_hz, hann_window, magnitude_spectrum, next_power_of_two, parabolic_offset};
use super::level::rms;

/// Octave-ish band edges in Hz, chosen for arcade audio rather than for music.
///
/// The bottom band captures the rumble a discrete explosion is supposed to have
/// and Donkey Kong's walk thump; the top captures LFSR hiss. Energy moving
/// between bands is a filter, mix or source difference; energy scaling in all
/// of them equally is a gain difference.
pub const BAND_EDGES_HZ: [f64; 6] = [0.0, 150.0, 400.0, 1000.0, 3000.0, 8000.0];

/// One peak in a magnitude spectrum.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Peak {
    pub hz: f64,
    /// Magnitude relative to the largest peak, so `1.0` is the strongest.
    pub relative: f64,
}

/// Frequency-domain description of a capture.
#[derive(Clone, Debug)]
pub struct Spectrum {
    /// Energy-weighted mean frequency. Brightness in one number.
    pub centroid_hz: f64,
    /// Frequency below which 85 % of the energy lies. Where the top end stops.
    pub rolloff_hz: f64,
    /// Geometric mean over arithmetic mean of the power spectrum. 0 is a pure
    /// tone, 1 is white noise. An "explosion" reading near zero is ringing like
    /// a bell rather than rumbling.
    pub flatness: f64,
    /// Best estimate of the fundamental, combining spectral and autocorrelation
    /// evidence. `None` when the signal has no periodicity to speak of.
    pub fundamental_hz: f64,
    /// The strongest spectral peaks, loudest first.
    pub peaks: Vec<Peak>,
    /// Fraction of total energy in each band of [`BAND_EDGES_HZ`], plus one
    /// final band above the last edge. Sums to 1.0.
    pub band_ratios: Vec<f64>,
}

impl Spectrum {
    /// Measure the DC-removed signal.
    ///
    /// Averages magnitude spectra over Hann-windowed frames rather than taking
    /// one transform of the whole capture: a single long FFT of a swept or
    /// gated effect smears everything together, and the average of frames is
    /// what the ear's integration time is closer to.
    pub fn measure(ac: &[f64], sample_rate: f64) -> Self {
        let avg = averaged_spectrum(ac, sample_rate);
        let n = next_power_of_two(analysis_window(ac.len()));

        Self {
            centroid_hz: spectral_centroid(&avg, n, sample_rate),
            rolloff_hz: spectral_rolloff(&avg, n, sample_rate, 0.85),
            flatness: spectral_flatness(&avg),
            fundamental_hz: fundamental_hz(ac, &avg, n, sample_rate),
            peaks: dominant_peaks(&avg, n, sample_rate, 5),
            band_ratios: band_energy_ratios(&avg, n, sample_rate),
        }
    }
}

/// Frame size for averaging. Long enough to resolve the bottom band edge
/// (150 Hz needs ~300 samples at 44.1 kHz, so 2048 is comfortable), short
/// enough that a 0.5 s effect still yields several frames.
fn analysis_window(len: usize) -> usize {
    2048.min(next_power_of_two(len.max(1)))
}

/// Magnitude spectrum averaged over overlapping Hann frames.
fn averaged_spectrum(ac: &[f64], _sample_rate: f64) -> Vec<f64> {
    let window = analysis_window(ac.len());
    if ac.len() < window {
        return magnitude_spectrum(ac);
    }
    let hop = window / 2;
    let w = hann_window(window);
    let mut sum: Vec<f64> = Vec::new();
    let mut frames = 0usize;
    let mut start = 0;
    while start + window <= ac.len() {
        let framed: Vec<f64> = ac[start..start + window]
            .iter()
            .zip(&w)
            .map(|(s, k)| s * k)
            .collect();
        let spec = magnitude_spectrum(&framed);
        if sum.is_empty() {
            sum = spec;
        } else {
            for (acc, v) in sum.iter_mut().zip(&spec) {
                *acc += v;
            }
        }
        frames += 1;
        start += hop;
    }
    if frames > 1 {
        for v in sum.iter_mut() {
            *v /= frames as f64;
        }
    }
    sum
}

/// Energy-weighted mean frequency of a magnitude spectrum.
pub fn spectral_centroid(spec: &[f64], n: usize, sample_rate: f64) -> f64 {
    let mut weighted = 0.0;
    let mut total = 0.0;
    for (i, &m) in spec.iter().enumerate() {
        let power = m * m;
        weighted += bin_hz(i, n, sample_rate) * power;
        total += power;
    }
    if total > 0.0 { weighted / total } else { 0.0 }
}

/// Frequency below which `fraction` of the total energy lies.
pub fn spectral_rolloff(spec: &[f64], n: usize, sample_rate: f64, fraction: f64) -> f64 {
    let total: f64 = spec.iter().map(|m| m * m).sum();
    if total <= 0.0 {
        return 0.0;
    }
    let target = total * fraction;
    let mut acc = 0.0;
    for (i, &m) in spec.iter().enumerate() {
        acc += m * m;
        if acc >= target {
            return bin_hz(i, n, sample_rate);
        }
    }
    bin_hz(spec.len().saturating_sub(1), n, sample_rate)
}

/// Geometric mean over arithmetic mean of the power spectrum.
///
/// Bin 0 is skipped: DC is not timbre, and on a DC-removed signal it is noise
/// near zero whose logarithm would dominate the geometric mean.
pub fn spectral_flatness(spec: &[f64]) -> f64 {
    let bins = &spec[1.min(spec.len())..];
    if bins.is_empty() {
        return 0.0;
    }
    // Floor before the log so an empty bin cannot send the product to zero and
    // report a pure tone as perfectly flat-free.
    const FLOOR: f64 = 1e-20;
    let mut log_sum = 0.0;
    let mut sum = 0.0;
    for &m in bins {
        let power = (m * m).max(FLOOR);
        log_sum += power.ln();
        sum += power;
    }
    let geo = (log_sum / bins.len() as f64).exp();
    let arith = sum / bins.len() as f64;
    if arith > 0.0 {
        (geo / arith).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// The strongest local maxima, loudest first, with sub-bin frequency
/// interpolation.
pub fn dominant_peaks(spec: &[f64], n: usize, sample_rate: f64, count: usize) -> Vec<Peak> {
    if spec.len() < 3 {
        return Vec::new();
    }
    let max = spec.iter().fold(0.0f64, |m, &v| m.max(v));
    if max <= 0.0 {
        return Vec::new();
    }

    let mut peaks: Vec<(f64, f64)> = Vec::new();
    for i in 1..spec.len() - 1 {
        if spec[i] > spec[i - 1] && spec[i] >= spec[i + 1] {
            let offset = parabolic_offset(spec[i - 1], spec[i], spec[i + 1]);
            let hz = bin_hz(i, n, sample_rate) + offset * sample_rate / n as f64;
            peaks.push((hz, spec[i] / max));
        }
    }
    peaks.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    peaks
        .into_iter()
        .take(count)
        .map(|(hz, relative)| Peak { hz, relative })
        .collect()
}

/// Best estimate of the fundamental frequency.
///
/// Autocorrelation leads because it locks to the period regardless of harmonic
/// structure — a square wave whose third harmonic is louder than its first
/// still autocorrelates at its own period, where "largest FFT bin" would report
/// the third harmonic. The spectral peak is the fallback and the sanity check:
/// if the two agree the answer is solid, and if autocorrelation finds no
/// periodicity at all (noise) the spectral peak is all there is.
pub fn fundamental_hz(ac: &[f64], spec: &[f64], n: usize, sample_rate: f64) -> f64 {
    let spectral = dominant_peaks(spec, n, sample_rate, 1)
        .first()
        .map(|p| p.hz)
        .unwrap_or(0.0);

    // Search 20 Hz .. 5 kHz, which covers every pitched arcade voice.
    let min_lag = (sample_rate / 5000.0).max(2.0) as usize;
    let max_lag = (sample_rate / 20.0) as usize;
    if ac.len() <= max_lag * 2 {
        return spectral;
    }

    // Use a bounded slice: autocorrelation over a whole multi-second capture
    // would average a swept tone into mush.
    let take = (max_lag * 4).min(ac.len());
    let corr = super::fft::autocorrelation(&ac[..take], max_lag);

    let best = (min_lag..=max_lag)
        .filter(|&l| corr[l] > corr[l - 1] && l < max_lag && corr[l] >= corr[l + 1])
        .max_by(|&a, &b| corr[a].partial_cmp(&corr[b]).unwrap());

    match best {
        // A weak autocorrelation peak means the signal is not really periodic,
        // so trust the spectrum instead.
        Some(lag) if corr[lag] > 0.3 => sample_rate / lag as f64,
        _ => spectral,
    }
}

/// Fraction of total energy in each band of [`BAND_EDGES_HZ`], plus a final
/// band above the last edge.
///
/// The load-bearing comparison between two captures: scale-invariant, so a gain
/// difference leaves every entry unchanged.
pub fn band_energy_ratios(spec: &[f64], n: usize, sample_rate: f64) -> Vec<f64> {
    let mut bands = vec![0.0; BAND_EDGES_HZ.len()];
    let mut total = 0.0;
    for (i, &m) in spec.iter().enumerate() {
        let hz = bin_hz(i, n, sample_rate);
        let power = m * m;
        total += power;
        let band = BAND_EDGES_HZ
            .iter()
            .rposition(|&edge| hz >= edge)
            .unwrap_or(0);
        bands[band] += power;
    }
    if total > 0.0 {
        for b in bands.iter_mut() {
            *b /= total;
        }
    }
    bands
}

/// Ratio of each harmonic's magnitude to the fundamental's, for `count`
/// harmonics starting at the second.
///
/// Separates square (odd harmonics, 1/n) from triangle (odd, 1/n²) from sine
/// (none) from a clipped or filtered version of any of them.
pub fn harmonic_ratios(
    spec: &[f64],
    n: usize,
    sample_rate: f64,
    f0: f64,
    count: usize,
) -> Vec<f64> {
    if f0 <= 0.0 {
        return vec![0.0; count];
    }
    let at = |hz: f64| -> f64 {
        let bin = (hz * n as f64 / sample_rate).round() as usize;
        // Take the local max: a harmonic rarely lands exactly on a bin centre.
        let lo = bin.saturating_sub(1);
        let hi = (bin + 2).min(spec.len());
        spec.get(lo..hi)
            .map(|w| w.iter().fold(0.0f64, |m, &v| m.max(v)))
            .unwrap_or(0.0)
    };
    let base = at(f0);
    if base <= 0.0 {
        return vec![0.0; count];
    }
    (2..count + 2).map(|h| at(f0 * h as f64) / base).collect()
}

/// Multi-resolution log-magnitude STFT distance between two signals.
///
/// The objective a fit minimizes, and the reason it is phase-blind: short
/// windows see attack and decay, long windows see steady-state tone, and log
/// magnitude matches perceived loudness so one loud band cannot dominate.
/// Returns 0.0 for identical inputs and grows with difference.
pub fn stft_distance(a: &[f64], b: &[f64], windows: &[usize]) -> f64 {
    const EPS: f64 = 1e-10;
    let mut total = 0.0;
    let mut counted = 0;
    for &w in windows {
        let hop = (w / 4).max(1);
        let fa = super::fft::stft(a, w, hop);
        let fb = super::fft::stft(b, w, hop);
        let frames = fa.len().min(fb.len());
        if frames == 0 {
            continue;
        }
        let mut sum = 0.0;
        let mut bins = 0usize;
        for (x, y) in fa.iter().take(frames).zip(fb.iter()) {
            for (p, q) in x.iter().zip(y) {
                sum += ((p + EPS).ln() - (q + EPS).ln()).abs();
                bins += 1;
            }
        }
        if bins > 0 {
            total += sum / bins as f64;
            counted += 1;
        }
    }
    if counted > 0 {
        total / counted as f64
    } else {
        0.0
    }
}

/// L1 distance between two RMS envelopes, normalized by length.
///
/// Scored separately from [`stft_distance`] so a loud frequency band cannot
/// hide an incorrect decay.
pub fn envelope_distance(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    a.iter()
        .zip(b)
        .take(n)
        .map(|(x, y)| (x - y).abs())
        .sum::<f64>()
        / n as f64
}

/// Sample offset that best aligns `b` to `a`, found by cross-correlating their
/// RMS envelopes within `±max_shift` envelope blocks.
///
/// Envelopes rather than waveforms on purpose: correlating raw samples of two
/// independent noise sources measures seed coincidence, not timing.
pub fn envelope_alignment(a: &[f64], b: &[f64], hop: usize, max_shift: usize) -> isize {
    let ea = super::level::rms_envelope(a, hop);
    let eb = super::level::rms_envelope(b, hop);
    if ea.is_empty() || eb.is_empty() {
        return 0;
    }
    let mut best = (0isize, f64::NEG_INFINITY);
    let max = max_shift as isize;
    for shift in -max..=max {
        let mut dot = 0.0;
        let mut n = 0usize;
        for (i, x) in ea.iter().enumerate() {
            let j = i as isize + shift;
            if j < 0 || j as usize >= eb.len() {
                continue;
            }
            dot += x * eb[j as usize];
            n += 1;
        }
        // Normalize by overlap so a large shift with few overlapping blocks
        // cannot win by accumulating less.
        let score = if n > 0 {
            dot / n as f64
        } else {
            f64::NEG_INFINITY
        };
        if score > best.1 {
            best = (shift, score);
        }
    }
    best.0 * hop as isize
}

/// Overall loudness ratio between two captures, as a linear gain.
pub fn gain_ratio(a: &[f64], b: &[f64]) -> f64 {
    let (ra, rb) = (rms(a), rms(b));
    if rb > 0.0 { ra / rb } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::analysis::tests::sine;
    use crate::audio::analysis::{analyze, remove_dc};

    fn white_noise(n: usize) -> Vec<f64> {
        // Deterministic LCG — a test must not depend on a random seed.
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        (0..n)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                ((state >> 33) as f64 / (1u64 << 31) as f64) - 1.0
            })
            .collect()
    }

    fn square(freq: f64, rate: f64, n: usize) -> Vec<f64> {
        (0..n)
            .map(|i| {
                let phase = (freq * i as f64 / rate).fract();
                if phase < 0.5 { 1.0 } else { -1.0 }
            })
            .collect()
    }

    /// The headline claim: a pure tone reads flatness ~0, white noise reads
    /// ~1. Without this the metric is just a number.
    #[test]
    fn flatness_separates_tone_from_noise() {
        let tone = analyze(&sine(1000.0, 44100.0, 44100), 44100.0);
        let noise = analyze(&white_noise(44100), 44100.0);
        assert!(
            tone.spectrum.flatness < 0.01,
            "tone {}",
            tone.spectrum.flatness
        );
        assert!(
            noise.spectrum.flatness > 0.3,
            "noise {}",
            noise.spectrum.flatness
        );
        assert!(noise.spectrum.flatness > tone.spectrum.flatness * 30.0);
    }

    #[test]
    fn centroid_tracks_pitch() {
        let low = analyze(&sine(200.0, 44100.0, 44100), 44100.0);
        let high = analyze(&sine(4000.0, 44100.0, 44100), 44100.0);
        assert!((low.spectrum.centroid_hz - 200.0).abs() < 20.0);
        assert!((high.spectrum.centroid_hz - 4000.0).abs() < 60.0);
    }

    /// A square wave's fundamental is its own period, not its loudest harmonic
    /// — the case that defeats a bare largest-bin estimator.
    #[test]
    fn fundamental_follows_the_period_not_the_loudest_bin() {
        for f in [110.0, 220.0, 440.0, 880.0] {
            let a = analyze(&square(f, 44100.0, 44100), 44100.0);
            let got = a.spectrum.fundamental_hz;
            assert!(
                (got - f).abs() < f * 0.02,
                "square at {f} Hz read as {got} Hz"
            );
        }
    }

    /// A square wave carries odd harmonics falling as 1/n; a triangle falls as
    /// 1/n². That difference is how a wrong waveform is spotted.
    #[test]
    fn harmonic_ratios_tell_square_from_sine() {
        let rate = 44100.0;
        let sq = remove_dc(&square(500.0, rate, 44100));
        let n = super::next_power_of_two(2048);
        let spec = super::averaged_spectrum(&sq, rate);
        let h = harmonic_ratios(&spec, n, rate, 500.0, 3);
        // Second harmonic absent (odd-only), third at about 1/3.
        assert!(h[0] < 0.15, "square 2nd harmonic {}", h[0]);
        assert!(
            (h[1] - 1.0 / 3.0).abs() < 0.12,
            "square 3rd harmonic {}",
            h[1]
        );

        let sn = remove_dc(&sine(500.0, rate, 44100));
        let spec = super::averaged_spectrum(&sn, rate);
        let h = harmonic_ratios(&spec, n, rate, 500.0, 3);
        assert!(
            h.iter().all(|&v| v < 0.05),
            "sine should have no harmonics: {h:?}"
        );
    }

    /// Band ratios are the scale-invariant column: halving the amplitude must
    /// not move them at all.
    #[test]
    fn band_ratios_are_gain_invariant() {
        let loud = analyze(&sine(500.0, 44100.0, 44100), 44100.0);
        let quiet: Vec<f64> = sine(500.0, 44100.0, 44100)
            .iter()
            .map(|s| s * 0.1)
            .collect();
        let quiet = analyze(&quiet, 44100.0);
        for (a, b) in loud
            .spectrum
            .band_ratios
            .iter()
            .zip(&quiet.spectrum.band_ratios)
        {
            assert!(
                (a - b).abs() < 1e-6,
                "band ratio moved with gain: {a} vs {b}"
            );
        }
        // But the level did change, which is the point of reporting both.
        assert!(loud.level.rms > quiet.level.rms * 5.0);
    }

    /// A filter difference moves energy between bands, which is what band
    /// ratios exist to catch. Bands are the half-open intervals between
    /// [`BAND_EDGES_HZ`], with a final open-ended band above the last edge, so
    /// 5 kHz lands in band 4 (3000..8000) and 10 kHz in band 5 (8000..).
    #[test]
    fn band_ratios_move_when_the_spectrum_does() {
        let low = analyze(&sine(100.0, 44100.0, 44100), 44100.0);
        let mid = analyze(&sine(5000.0, 44100.0, 44100), 44100.0);
        let high = analyze(&sine(10000.0, 44100.0, 44100), 44100.0);
        assert!(low.spectrum.band_ratios[0] > 0.9, "100 Hz should be band 0");
        assert!(mid.spectrum.band_ratios[4] > 0.9, "5 kHz should be band 4");
        assert!(
            high.spectrum.band_ratios[5] > 0.9,
            "10 kHz should be band 5"
        );
    }

    #[test]
    fn band_ratios_sum_to_one() {
        let a = analyze(&white_noise(44100), 44100.0);
        let sum: f64 = a.spectrum.band_ratios.iter().sum();
        assert!((sum - 1.0).abs() < 1e-9, "sum {sum}");
    }

    #[test]
    fn rolloff_sits_above_the_centroid_for_noise() {
        let a = analyze(&white_noise(44100), 44100.0);
        assert!(a.spectrum.rolloff_hz > a.spectrum.centroid_hz);
    }

    /// D(x, x) == 0, and D grows as a signal is detuned. Without both, the
    /// objective is not usable by an optimizer.
    #[test]
    fn stft_distance_is_zero_for_identical_and_grows_with_difference() {
        let rate = 44100.0;
        let reference = sine(1000.0, rate, 22050);
        let windows = [256, 1024, 4096];

        assert_eq!(stft_distance(&reference, &reference, &windows), 0.0);

        let mut last = 0.0;
        for detune in [1010.0, 1050.0, 1200.0, 2000.0] {
            let d = stft_distance(&reference, &sine(detune, rate, 22050), &windows);
            assert!(
                d > last,
                "distance did not grow at {detune} Hz: {d} <= {last}"
            );
            last = d;
        }
    }

    #[test]
    fn envelope_distance_is_zero_for_identical() {
        let env = vec![0.1, 0.5, 0.9, 0.4];
        assert_eq!(envelope_distance(&env, &env), 0.0);
        assert!(envelope_distance(&env, &[0.2, 0.5, 0.9, 0.4]) > 0.0);
    }

    /// A delayed copy is found at its delay, which is what lets two captures
    /// with different startup latency be compared.
    #[test]
    fn envelope_alignment_finds_a_known_delay() {
        let rate = 8000.0;
        let hop = 80; // 10 ms
        let burst = {
            let mut v = vec![0.0; 800];
            v.extend(sine(440.0, rate, 800));
            v.extend(vec![0.0; 2400]);
            v
        };
        let delayed = {
            let mut v = vec![0.0; 800 + 400];
            v.extend(sine(440.0, rate, 800));
            v.extend(vec![0.0; 2000]);
            v
        };
        let shift = envelope_alignment(&burst, &delayed, hop, 50);
        // b lags a by 400 samples, so a must shift forward to meet it.
        assert!((shift - 400).abs() <= hop as isize, "shift {shift}");
    }

    #[test]
    fn gain_ratio_measures_relative_loudness() {
        let a = sine(440.0, 8000.0, 8000);
        let b: Vec<f64> = a.iter().map(|s| s * 0.5).collect();
        assert!((gain_ratio(&a, &b) - 2.0).abs() < 1e-9);
        assert_eq!(gain_ratio(&a, &[0.0; 8000]), 0.0);
    }

    #[test]
    fn silence_produces_no_nans() {
        let a = analyze(&[0.0; 8192], 44100.0);
        assert!(a.spectrum.centroid_hz.is_finite());
        assert!(a.spectrum.flatness.is_finite());
        assert!(a.spectrum.rolloff_hz.is_finite());
        assert!(a.level.rms_dbfs.is_finite());
        assert!(a.spectrum.band_ratios.iter().all(|v| v.is_finite()));
    }
}
