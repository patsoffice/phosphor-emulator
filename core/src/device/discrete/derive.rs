//! Schematic values → node coefficients.
//!
//! Every builder method that takes real component values (ohms, farads, volts)
//! and stores something else on the node does that conversion here rather than
//! inline. Three reasons:
//!
//! 1. **The math gets tested directly.** A biquad derived from R/C values was
//!    previously only reachable through a built circuit's audio output, so a
//!    sign error in one coefficient could only be found by listening.
//! 2. **One derivation, one spelling.** [`rc_low_pass`] and [`low_pass_hz`] are
//!    two ways to describe the same one-pole filter; they now agree by
//!    construction rather than by inspection.
//! 3. **A parameter override has to reproduce it.** Changing a resistor after
//!    the fact means re-running the exact conversion the builder ran, which
//!    needs that conversion to be a callable function.
//!
//! Functions here are pure: schematic values (plus `dt` or `sim_rate` where the
//! discretization needs it) in, coefficients out. No node state, no builder.
//!
//! [`rc_low_pass`]: super::DiscreteCircuitBuilder::rc_low_pass
//! [`low_pass_hz`]: super::DiscreteCircuitBuilder::low_pass_hz

use std::f64::consts::{PI, TAU};

/// RC time constant in seconds: `τ = R·C`.
///
/// Shared by the one-pole low-pass and the high-pass / coupling capacitor —
/// they differ in how the node uses `τ`, not in how it is derived.
pub(crate) fn rc_tau(ohms: f64, farads: f64) -> f64 {
    ohms * farads
}

/// RC time constant for a one-pole filter specified by its corner frequency:
/// `τ = 1 / (2π·f_c)`.
pub(crate) fn tau_from_cutoff_hz(cutoff_hz: f64) -> f64 {
    1.0 / (TAU * cutoff_hz)
}

/// Per-step charge fraction for an RC settling toward its input:
/// `1 − e^(−dt/τ)`.
///
/// Used by the gated diode discharge (`rc_disc5`), where the cap follows the
/// input up instantly and decays with `τ = R·C`.
pub(crate) fn rc_charge_exp(ohms: f64, farads: f64, dt: f64) -> f64 {
    1.0 - (-dt / (ohms * farads)).exp()
}

/// `e^(−dt/τ)` — the fraction of the gap REMAINING after one step.
///
/// The complement of [`rc_charge_exp`], which is the fraction closed. Both are
/// called "the exponent" in circuit code and they are not interchangeable:
/// mixing them up moves a capacitor the whole way in one step instead of a
/// sliver, which reads as a stage that has no time constant at all.
pub(crate) fn rc_decay_exp(ohms: f64, farads: f64, dt: f64) -> f64 {
    (-dt / (ohms * farads)).exp()
}

/// Solve a CMOS inverter's transfer curve from its datasheet voltages.
///
/// The curve is `v_supply · exp(-a·(x/v_supply)^b)`, a monotonically decreasing
/// function of input — an inverter — with the two free parameters fixed by
/// requiring it to pass through both published threshold points: a falling input
/// reaching `v_in_rise` drives the output to `v_out_high`, and a rising input
/// reaching `v_in_fall` drives it to `v_out_low`.
///
/// This shape is not arbitrary. The gate's gain is what decides where a ring of
/// them switches, and therefore an oscillator's period; a step at mid-supply
/// misses the measured periods by ~20 %.
pub(crate) fn cmos_transfer_curve(gate: &super::CmosInverter) -> (f64, f64) {
    let vb = gate.v_supply;
    // ln(-ln(v/vb)) at each endpoint; the double log is what makes the fit
    // linear in ln(a) and b.
    let lo = (-(gate.v_out_low / vb).ln()).ln();
    let hi = (-(gate.v_out_high / vb).ln()).ln();
    let b = (lo - hi) / (gate.v_in_fall / gate.v_in_rise).ln();
    let a = (lo - b * (gate.v_in_fall / vb).ln()).exp();
    (a, b)
}

/// NE555 astable charge/discharge fractions per simulation step.
///
/// The cap charges through `r1 + r2` and discharges through `r2` alone, which
/// is what makes the free-running duty cycle asymmetric (>50 % high). Returns
/// `(exp_charge, exp_discharge)`.
pub(crate) fn ne555_astable_exponents(r1: f64, r2: f64, c: f64, dt: f64) -> (f64, f64) {
    (
        1.0 - (-dt / ((r1 + r2) * c)).exp(),
        1.0 - (-dt / (r2 * c)).exp(),
    )
}

/// The 555's internal resistor-divider thresholds: the comparator trips at
/// `2/3·Vcc` on the way up and `1/3·Vcc` on the way down. Returns
/// `(threshold, trigger)`.
pub(crate) fn ne555_thresholds(vcc: f64) -> (f64, f64) {
    (vcc * 2.0 / 3.0, vcc / 3.0)
}

/// The 555's default square-wave high level, `Vcc − 1.2 V` — the output stage's
/// saturation drop, not a rail.
pub(crate) fn ne555_default_out_high(vcc: f64) -> f64 {
    vcc - 1.2
}

/// Coefficients for an op-amp multiple-feedback band-pass built from real
/// component values.
///
/// `r_in` are the input resistors in parallel (the first carries the signal,
/// any others are references to `v_ref`), `rf` is the feedback resistor, and
/// `c1`/`c2` the feedback caps. Together they set center frequency, damping and
/// gain; the result is discretized with a pre-warped bilinear transform at
/// `sim_rate`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct OpAmpBandPass {
    pub a1: f64,
    pub a2: f64,
    pub b0: f64,
    pub b2: f64,
    pub in_gain: f64,
}

pub(crate) fn op_amp_band_pass_coeffs(
    r_in: &[f64],
    rf: f64,
    c1: f64,
    c2: f64,
    sim_rate: f64,
) -> OpAmpBandPass {
    let r_total = 1.0 / r_in.iter().map(|r| 1.0 / r).sum::<f64>();
    let fc = 1.0 / (TAU * (r_total * rf * c1 * c2).sqrt());
    let d = (c1 + c2) / (rf / r_total * c1 * c2).sqrt();
    let gain = -rf / r_total * c2 / (c1 + c2);

    // Pre-warped bilinear transform: match the analog corner exactly at `fc`
    // rather than letting the bilinear map compress it.
    let two_over_t = 2.0 * sim_rate;
    let two_over_t2 = two_over_t * two_over_t;
    let wc = sim_rate * 2.0 * (PI * fc / sim_rate).tan();
    let wc2 = wc * wc;
    let den = two_over_t2 + d * wc * two_over_t + wc2;
    let b0 = gain * (d * wc * two_over_t / den);

    OpAmpBandPass {
        a1: 2.0 * (-two_over_t2 + wc2) / den,
        a2: (two_over_t2 - d * wc * two_over_t + wc2) / den,
        b0,
        b2: -b0,
        in_gain: r_total / r_in[0],
    }
}

/// Passive resistor mixer: per-tap conductances and the total conductance
/// including any load resistor to the reference.
///
/// Output is `Σ(Vi·Gi) / G_total`, so the taps are a weighted average whose
/// weights come from `1/R`.
pub(crate) fn resistor_mixer_conductances(
    taps: &[(super::NodeId, f64)],
    load_ohms: Option<f64>,
) -> (Vec<(super::NodeId, f64)>, f64) {
    let srcs: Vec<(super::NodeId, f64)> = taps.iter().map(|(n, r)| (*n, 1.0 / r)).collect();
    let total_g = srcs.iter().map(|(_, g)| g).sum::<f64>() + load_ohms.map_or(0.0, |r| 1.0 / r);
    (srcs, total_g)
}

/// Per-bit weights for a linear R-2R ladder: bit `b` contributes
/// `vref·2^b / (2^bits − 1)`, so the full-scale code maps exactly to `vref`.
pub(crate) fn dac_r2r_weights(bits: u8, vref: f64) -> Vec<f64> {
    let full = ((1u64 << bits) - 1) as f64;
    (0..bits)
        .map(|b| vref * (1u64 << b) as f64 / full)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::NodeId;
    use super::*;

    /// The two spellings of a one-pole low-pass describe the same filter: an
    /// R/C pair and the corner frequency it produces must give one `τ`.
    #[test]
    fn rc_and_cutoff_spellings_agree() {
        let (r, c) = (2_200.0, 1e-6);
        let tau = rc_tau(r, c);
        let fc = 1.0 / (TAU * tau);
        assert!((tau_from_cutoff_hz(fc) - tau).abs() < 1e-15);
    }

    #[test]
    fn rc_tau_is_the_product() {
        assert_eq!(rc_tau(10_000.0, 1e-7), 1e-3);
    }

    /// A step response settles to 63.2 % of the way after exactly one τ, which
    /// is the definition of the time constant.
    #[test]
    fn charge_exp_reaches_one_tau_at_63_percent() {
        let (r, c) = (1_000.0, 1e-6);
        let tau = rc_tau(r, c);
        // Integrate the one-pole toward 1.0 for exactly τ seconds.
        let steps = 10_000;
        let dt = tau / steps as f64;
        let k = rc_charge_exp(r, c, dt);
        let mut v = 0.0;
        for _ in 0..steps {
            v += (1.0 - v) * k;
        }
        assert!((v - 0.632_120_558).abs() < 1e-6, "settled to {v}");
    }

    /// A shorter step charges less per step; the fraction is monotonic in dt.
    #[test]
    fn charge_exp_is_monotonic_in_dt() {
        let slow = rc_charge_exp(1_000.0, 1e-6, 1e-7);
        let fast = rc_charge_exp(1_000.0, 1e-6, 1e-6);
        assert!(slow < fast);
        assert!(slow > 0.0 && fast < 1.0);
    }

    /// The cap charges through R1+R2 and discharges through R2, so the charge
    /// leg is always the slower one — the source of the >50 % duty cycle.
    #[test]
    fn ne555_charges_slower_than_it_discharges() {
        let (charge, discharge) =
            ne555_astable_exponents(47_000.0, 27_000.0, 33e-9, 1.0 / 192_000.0);
        assert!(charge < discharge, "charge {charge} discharge {discharge}");
        assert!(charge > 0.0 && discharge < 1.0);
    }

    /// Ideal RC analysis puts the free-running frequency at
    /// `1 / (ln2·(R1 + 2·R2)·C)`: the cap crosses 1/3→2/3 Vcc charging through
    /// `R1+R2` and 2/3→1/3 discharging through `R2`, and each leg takes `ln2`
    /// time constants. Simulating with the derived exponents must reproduce
    /// that closely, since it is exactly the integration the node performs.
    ///
    /// Note this is `1.4427/((R1+2R2)C)`, not the `1.49` the datasheet quotes
    /// and the older test above compares against with a 10 % tolerance — the
    /// real chip runs a few percent fast. The model is ideal, so pin the ideal
    /// number here and track the discrepancy separately.
    #[test]
    fn ne555_free_runs_at_the_ideal_rc_frequency() {
        let (r1, r2, c, vcc) = (47_000.0, 27_000.0, 33e-9, 5.0);
        let sim_rate = 1_000_000.0;
        let dt = 1.0 / sim_rate;
        let (charge, discharge) = ne555_astable_exponents(r1, r2, c, dt);
        let (threshold, trigger) = ne555_thresholds(vcc);

        let mut cap = 0.0;
        let mut high = true;
        let mut edges = 0;
        let steps = sim_rate as usize; // one second
        for _ in 0..steps {
            if high {
                cap += (vcc - cap) * charge;
                if cap >= threshold {
                    high = false;
                }
            } else {
                cap -= cap * discharge;
                if cap <= trigger {
                    high = true;
                    edges += 1;
                }
            }
        }
        let expected = 1.0 / (std::f64::consts::LN_2 * (r1 + 2.0 * r2) * c);
        let ratio = edges as f64 / expected;
        assert!(
            (0.99..1.01).contains(&ratio),
            "measured {edges} Hz vs ideal {expected:.1} Hz"
        );
    }

    #[test]
    fn ne555_thresholds_are_the_internal_divider() {
        let (threshold, trigger) = ne555_thresholds(5.0);
        assert!((threshold - 10.0 / 3.0).abs() < 1e-12);
        assert!((trigger - 5.0 / 3.0).abs() < 1e-12);
        assert_eq!(ne555_default_out_high(5.0), 3.8);
    }

    /// The band-pass must peak at the center frequency its R/C values set, and
    /// roll off on both sides. Sweep a sine through the discretized biquad and
    /// find where the response is largest.
    #[test]
    fn op_amp_band_pass_peaks_at_its_center_frequency() {
        // Asteroids thrust: fc ≈ 89.5 Hz, Q ≈ 7.6.
        let r_in = [1_170.0];
        let (rf, c1, c2) = (270_000.0, 0.1e-6, 0.1e-6);
        let sim_rate = 192_000.0;
        let k = op_amp_band_pass_coeffs(&r_in, rf, c1, c2, sim_rate);

        let expected_fc = {
            let r_total = 1.0 / r_in.iter().map(|r| 1.0 / r).sum::<f64>();
            1.0 / (TAU * (r_total * rf * c1 * c2).sqrt())
        };

        let response = |f: f64| {
            let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
            let mut peak: f64 = 0.0;
            let n = (sim_rate * 2.0) as usize;
            for i in 0..n {
                let x = (TAU * f * i as f64 / sim_rate).sin();
                let y = k.b0 * x + k.b2 * x2 - k.a1 * y1 - k.a2 * y2;
                x2 = x1;
                x1 = x;
                y2 = y1;
                y1 = y;
                // Ignore the settling transient; a Q of 7.6 rings for a while.
                if i > n / 2 {
                    peak = peak.max(y.abs());
                }
            }
            peak
        };

        let at_center = response(expected_fc);
        assert!(
            at_center > response(expected_fc / 4.0) * 2.0,
            "no low rolloff"
        );
        assert!(
            at_center > response(expected_fc * 4.0) * 2.0,
            "no high rolloff"
        );
    }

    /// A single input resistor is the whole parallel network, so the input gain
    /// term is unity; a second reference resistor splits it.
    #[test]
    fn op_amp_band_pass_in_gain_is_the_input_divider() {
        let one = op_amp_band_pass_coeffs(&[1_000.0], 100_000.0, 1e-7, 1e-7, 192_000.0);
        assert!((one.in_gain - 1.0).abs() < 1e-12);

        let two = op_amp_band_pass_coeffs(&[1_000.0, 1_000.0], 100_000.0, 1e-7, 1e-7, 192_000.0);
        assert!((two.in_gain - 0.5).abs() < 1e-12);
    }

    /// `b2` mirrors `b0`: the band-pass numerator is `b0·(1 − z⁻²)`, which is
    /// what puts zeros at DC and Nyquist.
    #[test]
    fn op_amp_band_pass_zeros_sit_at_dc_and_nyquist() {
        let k = op_amp_band_pass_coeffs(&[1_170.0], 270_000.0, 1e-7, 1e-7, 192_000.0);
        assert_eq!(k.b2, -k.b0);
    }

    #[test]
    fn resistor_mixer_weights_are_reciprocal_resistances() {
        let a = NodeId(0);
        let b = NodeId(1);
        let (srcs, total) = resistor_mixer_conductances(&[(a, 1_000.0), (b, 2_000.0)], None);
        assert_eq!(srcs[0].1, 1e-3);
        assert_eq!(srcs[1].1, 5e-4);
        assert!((total - 1.5e-3).abs() < 1e-15);
    }

    /// A load resistor to the reference adds conductance without adding a tap,
    /// so it attenuates every input equally.
    #[test]
    fn resistor_mixer_load_attenuates_without_a_tap() {
        let a = NodeId(0);
        let (unloaded, g_unloaded) = resistor_mixer_conductances(&[(a, 1_000.0)], None);
        let (loaded, g_loaded) = resistor_mixer_conductances(&[(a, 1_000.0)], Some(1_000.0));
        assert_eq!(unloaded.len(), loaded.len());
        assert!((unloaded[0].1 / g_unloaded - 1.0).abs() < 1e-15);
        assert!((loaded[0].1 / g_loaded - 0.5).abs() < 1e-15);
    }

    /// Full-scale code lands exactly on vref, and each bit is worth twice the
    /// one below it.
    #[test]
    fn dac_r2r_full_scale_is_vref() {
        let w = dac_r2r_weights(8, 5.0);
        assert_eq!(w.len(), 8);
        let full: f64 = w.iter().sum();
        assert!((full - 5.0).abs() < 1e-12);
        for pair in w.windows(2) {
            assert!((pair[1] / pair[0] - 2.0).abs() < 1e-12);
        }
    }
}
