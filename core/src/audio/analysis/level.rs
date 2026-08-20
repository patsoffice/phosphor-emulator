//! Signal integrity, level, and how level evolves over time.
//!
//! Split from the spectral metrics because they answer different questions and
//! because a loud frequency band must not be able to hide a wrong decay. An
//! effect whose spectrum matches perfectly and whose tail is twice too long is
//! wrong in a way only this file can see.

use super::fft::db;

/// Anything above this fraction of full scale counts as a clipped sample.
///
/// Not exactly 1.0: a converter that has hit the rail usually reports a value a
/// hair below it, and a run of samples pinned at 0.999 is clipping by any
/// useful definition.
const CLIP_THRESHOLD: f64 = 0.999;

/// A capture whose peak never reaches this is treated as silent.
///
/// One LSB of 16-bit is about 3e-5; this is comfortably above the noise floor
/// of a dithered capture but far below anything audible.
const SILENCE_THRESHOLD: f64 = 1e-4;

/// Is this capture usable at all?
///
/// These are defects rather than differences. A capture with a large DC offset
/// or a third of its samples pinned at the rail is not something to compare
/// against a reference — it is something to fix first.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Integrity {
    /// Mean sample value. Should be near zero; a large offset means a stuck
    /// source, a wrong bias, or a missing coupling capacitor.
    pub dc_offset: f64,
    /// Largest absolute sample.
    pub peak: f64,
    /// Peak expressed in dBFS.
    pub peak_dbfs: f64,
    /// Number of samples at or beyond [`CLIP_THRESHOLD`].
    pub clipped: usize,
    /// Clipped samples as a fraction of the whole.
    pub clipped_fraction: f64,
    /// Peak divided by RMS. High means transient, near 1.0 means the signal is
    /// compressed flat — a square wave is 1.0, a sine is 1.41.
    pub crest_factor: f64,
    /// Fraction of the capture below [`SILENCE_THRESHOLD`], measured over short
    /// blocks rather than per sample so a zero crossing does not count.
    pub silent_fraction: f64,
    /// True when the whole capture is below the silence threshold.
    pub is_silent: bool,
}

impl Integrity {
    /// Measure the *raw* signal — do not remove DC first, since the offset is
    /// one of the things being reported.
    pub fn measure(samples: &[f64]) -> Self {
        if samples.is_empty() {
            return Self {
                dc_offset: 0.0,
                peak: 0.0,
                peak_dbfs: db(0.0),
                clipped: 0,
                clipped_fraction: 0.0,
                crest_factor: 0.0,
                silent_fraction: 1.0,
                is_silent: true,
            };
        }

        let dc = dc_offset(samples);
        let peak = samples.iter().fold(0.0f64, |m, s| m.max(s.abs()));
        let clipped = samples.iter().filter(|s| s.abs() >= CLIP_THRESHOLD).count();

        // Crest factor is about waveform shape, so measure it on the AC part —
        // otherwise a DC offset inflates RMS and flattens the ratio.
        let ac_rms = rms(&samples.iter().map(|s| s - dc).collect::<Vec<_>>());
        let crest = if ac_rms > 0.0 { peak / ac_rms } else { 0.0 };

        // Block-wise silence: a sine crosses zero constantly, so counting bare
        // samples below the threshold would call every signal partly silent.
        const BLOCK: usize = 256;
        let blocks = samples.chunks(BLOCK);
        let total_blocks = blocks.len();
        let silent_blocks = samples
            .chunks(BLOCK)
            .filter(|b| b.iter().all(|s| s.abs() < SILENCE_THRESHOLD))
            .count();

        Self {
            dc_offset: dc,
            peak,
            peak_dbfs: db(peak),
            clipped,
            clipped_fraction: clipped as f64 / samples.len() as f64,
            crest_factor: crest,
            silent_fraction: silent_blocks as f64 / total_blocks.max(1) as f64,
            is_silent: peak < SILENCE_THRESHOLD,
        }
    }
}

/// Level, and how it changes over time.
#[derive(Clone, Debug)]
pub struct Level {
    /// RMS of the DC-removed signal.
    pub rms: f64,
    /// The same in dBFS.
    pub rms_dbfs: f64,
    /// Sum of squares — the total strength of a one-shot effect, which RMS
    /// understates because it averages the silent tail in.
    pub integrated_energy: f64,
    /// RMS in successive short blocks, at [`Level::ENVELOPE_HOP_S`] resolution.
    /// This is the measurement that sees attack and decay.
    pub envelope: Vec<f64>,
    /// Seconds from the start of the capture to the detected onset.
    pub onset_s: Option<f64>,
    /// Seconds for the envelope to fall 20 dB from its peak.
    pub decay_t20_s: Option<f64>,
    /// Seconds for the envelope to fall 40 dB from its peak.
    pub decay_t40_s: Option<f64>,
    /// Fitted decay time constant in seconds, and the fit's r². The fit is the
    /// trustworthy decay number on a noisy voice; T20 and T40 read two points
    /// off the same curve and a shift-register-gated effect moves those points
    /// around. Check the r² before believing the tau.
    pub decay_tau_s: Option<(f64, f64)>,
    /// Seconds the envelope spends within 40 dB of its peak.
    pub duration_above_threshold_s: f64,
}

impl Level {
    /// Envelope resolution. 5 ms is short enough to catch an arcade effect's
    /// attack and long enough that a 100 Hz tone still fills a block.
    pub const ENVELOPE_HOP_S: f64 = 0.005;

    /// Measure the DC-removed signal.
    pub fn measure(ac: &[f64], sample_rate: f64) -> Self {
        let hop = ((sample_rate * Self::ENVELOPE_HOP_S) as usize).max(1);
        let envelope = rms_envelope(ac, hop);
        let peak_env = envelope.iter().fold(0.0f64, |m, &v| m.max(v));

        // Everything below is measured from the envelope peak, so a capture
        // with no signal at all has no meaningful decay or duration.
        let (t20, t40, tau, above) = if peak_env > 0.0 {
            let hop_s = hop as f64 / sample_rate;
            let above_count = envelope
                .iter()
                .filter(|&&v| db(v / peak_env) > -40.0)
                .count();
            (
                decay_time(&envelope, hop_s, 20.0),
                decay_time(&envelope, hop_s, 40.0),
                // Fitted over the top 30 dB: below that a one-shot's tail is
                // into the capture's noise floor and only flattens the slope.
                decay_tau(&envelope, hop_s, -30.0),
                above_count as f64 * hop_s,
            )
        } else {
            (None, None, None, 0.0)
        };

        Self {
            rms: rms(ac),
            rms_dbfs: rms_dbfs(ac),
            integrated_energy: integrated_energy(ac),
            envelope,
            onset_s: onset_index(ac, sample_rate).map(|i| i as f64 / sample_rate),
            decay_t20_s: t20,
            decay_t40_s: t40,
            decay_tau_s: tau,
            duration_above_threshold_s: above,
        }
    }
}

/// Mean sample value.
pub fn dc_offset(samples: &[f64]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    samples.iter().sum::<f64>() / samples.len() as f64
}

/// Root mean square.
pub fn rms(samples: &[f64]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f64>() / samples.len() as f64).sqrt()
}

/// RMS in dBFS, floored rather than `-inf` for silence.
pub fn rms_dbfs(samples: &[f64]) -> f64 {
    db(rms(samples))
}

/// Sum of squares.
///
/// The right level measure for a one-shot: two explosions of equal loudness but
/// different lengths have different RMS over a fixed window and the same
/// integrated energy only if they truly carry the same energy.
pub fn integrated_energy(samples: &[f64]) -> f64 {
    samples.iter().map(|s| s * s).sum()
}

/// RMS of each successive `hop`-sample block.
pub fn rms_envelope(samples: &[f64], hop: usize) -> Vec<f64> {
    assert!(hop > 0, "envelope hop must be positive");
    samples.chunks(hop).map(rms).collect()
}

/// First sample index where the signal rises decisively above its own noise
/// floor, or `None` if it never does.
///
/// The floor is estimated from the quietest tenth of the envelope rather than
/// assumed, so this works on a capture with a noisy baseline as well as one
/// that starts from digital silence.
pub fn onset_index(ac: &[f64], sample_rate: f64) -> Option<usize> {
    let hop = ((sample_rate * 0.001) as usize).max(1);
    let env = rms_envelope(ac, hop);
    if env.is_empty() {
        return None;
    }
    let peak = env.iter().fold(0.0f64, |m, &v| m.max(v));
    if peak <= 0.0 {
        return None;
    }

    let mut sorted = env.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let floor = sorted[sorted.len() / 10];

    // Ten times the floor, but never less than 1 % of the peak: the first keeps
    // a noisy baseline from triggering, the second keeps a digitally silent one
    // from making the threshold zero.
    let threshold = (floor * 10.0).max(peak * 0.01);
    env.iter().position(|&v| v >= threshold).map(|b| b * hop)
}

/// The envelope's non-increasing upper hull from `start`: `hull[i]` is the
/// largest value at or after `i`.
///
/// A noisy voice's envelope fluctuates hard. An explosion gated by a shift
/// register is a narrowband noise burst whose block RMS wanders by many dB
/// around its trend, so it dips far below that trend and comes straight back.
/// Taking the hull answers "has it fallen this far *and stayed* there", which is
/// what a decay time means, instead of "did it ever momentarily dip".
fn decay_hull(envelope: &[f64], start: usize) -> Vec<f64> {
    let mut hull: Vec<f64> = envelope[start..].to_vec();
    for i in (0..hull.len().saturating_sub(1)).rev() {
        hull[i] = hull[i].max(hull[i + 1]);
    }
    hull
}

/// Time in seconds for an envelope to fall `drop_db` below its peak, measured
/// from the peak forward.
///
/// Returns `None` when the signal never falls that far — common for short
/// arcade effects, which is exactly why T20 and T40 are reported instead of a
/// single T60 that would usually be unavailable.
///
/// Measured on the envelope's upper hull, not on its raw first crossing. The
/// raw version reported Galaxian's explosion decaying in 0.524 s against the
/// board's 0.818 s, and both were wrong: the voice's true time constant is
/// 0.75 s, which is a T20 of 1.75 s. What it had found was a statistical dip in
/// a noise envelope, and the bias is one-sided because the peak it measures
/// down from is itself the maximum of the same fluctuation. That reading
/// survived long enough to be written up as a board's last outstanding
/// residual, so this is not a rounding-level concern.
pub fn decay_time(envelope: &[f64], hop_s: f64, drop_db: f64) -> Option<f64> {
    let (peak_i, peak) = envelope
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())?;
    if *peak <= 0.0 {
        return None;
    }
    let target = peak * 10f64.powf(-drop_db / 20.0);
    decay_hull(envelope, peak_i)
        .iter()
        .position(|&v| v <= target)
        .map(|d| d as f64 * hop_s)
}

/// Least-squares time constant of an envelope's decay, in seconds, with the
/// coefficient of determination of the fit.
///
/// Fitted across the whole decay rather than read off two crossings, so noise
/// averages out instead of choosing the answer. `r2` is what makes the number
/// safe to use: a genuine exponential fits near 1.0, and anything that is not
/// exponential (a two-stage decay, a sustained tone, a signal still in its
/// attack) reports a low value rather than a confident wrong time constant.
///
/// Fitted from the envelope peak forward over the samples above `floor_db`
/// below the peak, since once the tail reaches the noise floor it stops
/// carrying decay information and would flatten the slope.
pub fn decay_tau(envelope: &[f64], hop_s: f64, floor_db: f64) -> Option<(f64, f64)> {
    let (peak_i, peak) = envelope
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())?;
    if *peak <= 0.0 {
        return None;
    }
    let floor = peak * 10f64.powf(floor_db / 20.0);
    let pts: Vec<(f64, f64)> = envelope[peak_i..]
        .iter()
        .enumerate()
        .filter(|(_, v)| **v > floor && **v > 0.0)
        .map(|(i, v)| (i as f64 * hop_s, v.ln()))
        .collect();
    // Three points can be fitted but say nothing; below this the fit is noise.
    if pts.len() < 8 {
        return None;
    }

    let n = pts.len() as f64;
    let sx: f64 = pts.iter().map(|p| p.0).sum();
    let sy: f64 = pts.iter().map(|p| p.1).sum();
    let sxx: f64 = pts.iter().map(|p| p.0 * p.0).sum();
    let sxy: f64 = pts.iter().map(|p| p.0 * p.1).sum();
    let denom = n * sxx - sx * sx;
    if denom.abs() < f64::EPSILON {
        return None;
    }
    let slope = (n * sxy - sx * sy) / denom;
    // A flat or rising envelope is not a decay.
    if slope >= 0.0 {
        return None;
    }
    let intercept = (sy - slope * sx) / n;

    let mean_y = sy / n;
    let ss_tot: f64 = pts.iter().map(|p| (p.1 - mean_y).powi(2)).sum();
    let ss_res: f64 = pts
        .iter()
        .map(|p| (p.1 - (slope * p.0 + intercept)).powi(2))
        .sum();
    let r2 = if ss_tot > 0.0 {
        1.0 - ss_res / ss_tot
    } else {
        0.0
    };
    // Below this the envelope is not describable as one exponential and no
    // number should be offered. A sustained voice drifts just enough to produce
    // a slope, and reporting it gave a 143-second time constant next to an r² of
    // zero. The r² was doing its job, but a number that meaningless should not
    // be printed at all: it invites exactly the misreading this metric exists to
    // prevent.
    const MIN_FIT_R2: f64 = 0.5;
    (r2 >= MIN_FIT_R2).then_some((-1.0 / slope, r2))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::analysis::tests::sine;

    #[test]
    fn a_full_scale_sine_is_minus_three_dbfs() {
        let s = sine(1000.0, 44100.0, 44100);
        assert!((rms(&s) - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-3);
        assert!((rms_dbfs(&s) + 3.0103).abs() < 0.01);
    }

    #[test]
    fn empty_input_does_not_panic_or_return_infinity() {
        let i = Integrity::measure(&[]);
        assert!(i.is_silent);
        assert!(i.peak_dbfs.is_finite());
        assert_eq!(rms(&[]), 0.0);
        assert_eq!(dc_offset(&[]), 0.0);
    }

    /// A DC offset must show up as an offset, not be quietly absorbed.
    #[test]
    fn dc_offset_is_reported_not_hidden() {
        let biased: Vec<f64> = sine(440.0, 8000.0, 8000)
            .iter()
            .map(|s| s * 0.1 + 0.4)
            .collect();
        let i = Integrity::measure(&biased);
        assert!((i.dc_offset - 0.4).abs() < 1e-3);
        // And it inflates the peak, which is the audible consequence.
        assert!(i.peak > 0.49);
    }

    #[test]
    fn clipping_is_counted() {
        let mut s = sine(100.0, 8000.0, 8000);
        for v in s.iter_mut() {
            *v = (*v * 3.0).clamp(-1.0, 1.0);
        }
        let i = Integrity::measure(&s);
        assert!(i.clipped > 0);
        // Driving a sine 3x into the rails flattens most of it.
        assert!(i.clipped_fraction > 0.5, "{}", i.clipped_fraction);
    }

    /// Crest factor tells waveform shapes apart: a square is pinned at its
    /// peak, a sine averages lower, and an impulse is almost all peak.
    #[test]
    fn crest_factor_separates_square_from_sine_from_impulse() {
        let square: Vec<f64> = (0..8000)
            .map(|i| if (i / 40) % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        let sq = Integrity::measure(&square).crest_factor;
        assert!((sq - 1.0).abs() < 0.01, "square crest {sq}");

        let sn = Integrity::measure(&sine(100.0, 8000.0, 8000)).crest_factor;
        assert!(
            (sn - std::f64::consts::SQRT_2).abs() < 0.01,
            "sine crest {sn}"
        );

        let mut impulse = vec![0.0; 8000];
        impulse[100] = 1.0;
        assert!(Integrity::measure(&impulse).crest_factor > 50.0);
    }

    #[test]
    fn silence_is_detected_but_a_quiet_signal_is_not() {
        assert!(Integrity::measure(&[0.0; 4096]).is_silent);
        let quiet: Vec<f64> = sine(440.0, 8000.0, 8000).iter().map(|s| s * 0.01).collect();
        let i = Integrity::measure(&quiet);
        assert!(!i.is_silent);
        assert_eq!(i.silent_fraction, 0.0);
    }

    /// A signal that is silent for its first half reports about half silent —
    /// measured block-wise, so the sine's zero crossings do not count.
    #[test]
    fn silent_fraction_measures_blocks_not_zero_crossings() {
        let mut s = vec![0.0; 4096];
        s.extend(sine(440.0, 8000.0, 4096));
        let f = Integrity::measure(&s).silent_fraction;
        assert!((f - 0.5).abs() < 0.02, "silent fraction {f}");
    }

    /// The measurement the whole file exists for: an exponential decay's T20
    /// and T40 must match the time constant that generated it.
    #[test]
    fn decay_times_match_a_known_exponential() {
        let rate = 8000.0;
        // Amplitude e^(-t/tau); -20 dB is a factor of 10, so t20 = tau*ln(10).
        let tau = 0.25;
        let sig: Vec<f64> = (0..(rate as usize * 3))
            .map(|i| {
                let t = i as f64 / rate;
                (-t / tau).exp() * (std::f64::consts::TAU * 300.0 * t).sin()
            })
            .collect();
        let level = Level::measure(&sig, rate);

        let expect_t20 = tau * 10f64.ln();
        let expect_t40 = tau * 100f64.ln();
        let t20 = level.decay_t20_s.expect("t20");
        let t40 = level.decay_t40_s.expect("t40");
        assert!((t20 - expect_t20).abs() < 0.03, "t20 {t20} vs {expect_t20}");
        assert!((t40 - expect_t40).abs() < 0.03, "t40 {t40} vs {expect_t40}");
    }

    /// A sustained tone never decays, so a decay time is genuinely absent
    /// rather than reported as the end of the capture. Neither is a time
    /// constant: a sustained voice drifts just enough to fit a slope, and a
    /// galaxian background capture produced a 143-second tau that way.
    #[test]
    fn a_sustained_tone_has_no_decay_time() {
        let level = Level::measure(&sine(440.0, 8000.0, 8000), 8000.0);
        assert!(level.decay_t20_s.is_none());
        assert!(
            level.decay_tau_s.is_none(),
            "a sustained tone reported a time constant: {:?}",
            level.decay_tau_s
        );
    }

    /// The fitted time constant recovers the tau that generated the signal.
    #[test]
    fn fitted_tau_matches_a_known_exponential() {
        let rate = 8000.0;
        let tau = 0.25;
        let sig: Vec<f64> = (0..(rate as usize * 3))
            .map(|i| {
                let t = i as f64 / rate;
                (-t / tau).exp() * (std::f64::consts::TAU * 300.0 * t).sin()
            })
            .collect();
        let (fitted, r2) = Level::measure(&sig, rate).decay_tau_s.expect("tau");
        assert!((fitted - tau).abs() < 0.02, "tau {fitted} vs {tau}");
        assert!(r2 > 0.99, "a clean exponential should fit tightly, r2={r2}");
    }

    /// THE DEFECT THIS EXISTS FOR. A decay carried by noise, as every
    /// shift-register-gated explosion on these boards is, has an envelope that
    /// fluctuates hard around its trend. Reading the first crossing of a
    /// threshold finds one of those dips and reports a decay several times too
    /// fast; on Galaxian's explosion it reported 0.524 s where the voice's true
    /// time constant of 0.75 s means a T20 of 1.75 s.
    ///
    /// Both measurements have to survive the noise: T20 because it is quoted
    /// everywhere, and the fitted tau because it is the number to trust.
    #[test]
    fn a_noisy_decay_is_not_cut_short_by_a_dip_in_its_envelope() {
        let rate = 44100.0;
        let tau = 0.75;
        // A deterministic LFSR gate at 7920 Hz, which is the real mechanism:
        // the voice is a capacitor voltage chopped by a noise line, so about
        // half its samples are legitimately zero at any amplitude.
        let mut lfsr: u32 = 0x1_ACE1;
        let mut gate = false;
        let mut next = 0.0;
        let sig: Vec<f64> = (0..(rate as usize * 3))
            .map(|i| {
                let t = i as f64 / rate;
                if t >= next {
                    next += 1.0 / 7920.0;
                    let bit = ((lfsr >> 16) ^ (lfsr >> 13)) & 1;
                    lfsr = ((lfsr << 1) | bit) & 0x1_FFFF;
                    gate = bit != 0;
                }
                let carrier = (std::f64::consts::TAU * 170.0 * t).sin();
                if gate {
                    (-t / tau).exp() * carrier
                } else {
                    0.0
                }
            })
            .collect();

        let level = Level::measure(&sig, rate);

        let (fitted, r2) = level.decay_tau_s.expect("tau");
        assert!(
            (fitted - tau).abs() < 0.10,
            "fitted tau {fitted} should recover {tau} through the gating"
        );
        assert!(r2 > 0.85, "a gated exponential still fits well, r2={r2}");

        // T20 of a tau=0.75 decay is tau*ln(10) = 1.727 s. The pre-fix code
        // returned a small fraction of that, so a loose bound still pins it.
        let t20 = level.decay_t20_s.expect("t20");
        let expect = tau * 10f64.ln();
        assert!(
            (t20 - expect).abs() < 0.25,
            "t20 {t20} should be near {expect}, not an envelope dip"
        );
    }

    /// A decay that is not a single exponential must report a poor fit rather
    /// than a confident wrong time constant. Two stacked decays are the common
    /// case: an effect whose envelope and its carrier's filter both ring.
    #[test]
    fn a_two_stage_decay_reports_a_poor_fit() {
        let rate = 8000.0;
        let sig: Vec<f64> = (0..(rate as usize * 3))
            .map(|i| {
                let t = i as f64 / rate;
                // The slow stage has to sit ABOVE the fit's -30 dB floor to be
                // part of what is fitted; at 0.02 it is below the floor and the
                // fit sees only the clean fast stage, which of course fits.
                let env = (-t / 0.05).exp() + 0.15 * (-t / 1.5).exp();
                env * (std::f64::consts::TAU * 300.0 * t).sin()
            })
            .collect();
        let (_, r2) = Level::measure(&sig, rate).decay_tau_s.expect("tau");
        assert!(
            r2 < 0.95,
            "a two-stage decay should not fit cleanly, r2={r2}"
        );
    }

    #[test]
    fn onset_is_found_after_leading_silence() {
        let rate = 8000.0;
        let mut s = vec![0.0; 4000]; // 0.5 s
        s.extend(sine(440.0, rate, 4000));
        let onset = onset_index(&s, rate).expect("onset") as f64 / rate;
        assert!((onset - 0.5).abs() < 0.01, "onset at {onset}");
    }

    #[test]
    fn silence_has_no_onset() {
        assert!(onset_index(&[0.0; 4096], 8000.0).is_none());
    }

    /// Integrated energy scales with duration where RMS does not — the reason
    /// both are reported for one-shots.
    #[test]
    fn integrated_energy_tracks_duration_where_rms_does_not() {
        let short = sine(440.0, 8000.0, 1000);
        let long = sine(440.0, 8000.0, 2000);
        assert!((rms(&short) - rms(&long)).abs() < 1e-3);
        let ratio = integrated_energy(&long) / integrated_energy(&short);
        assert!((ratio - 2.0).abs() < 0.05, "energy ratio {ratio}");
    }

    #[test]
    fn envelope_resolution_follows_the_hop() {
        let env = rms_envelope(&[1.0; 1000], 100);
        assert_eq!(env.len(), 10);
        assert!(env.iter().all(|&v| (v - 1.0).abs() < 1e-12));
    }
}
