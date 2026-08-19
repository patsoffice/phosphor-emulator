//! Primitive components for the discrete sound circuit graph.
//!
//! A [`Node`] pairs a primitive [`NodeKind`] with the per-node scheduler state
//! used by the circuit's clock domains. Topology (which node reads which) lives
//! in the `NodeId` references inside each kind and is fixed once the circuit is
//! built; everything mutated at runtime is serialized by `save_runtime`.

use super::{ClockDomain, CustomComponent, FilterMode, NodeId, Output555};
use crate::core::save_state::{SaveError, StateReader, StateWriter};

/// One node in the circuit graph: a primitive kind plus per-node scheduler
/// state (Bresenham phase for rate-limited domains, last-seen input generation
/// for the event-driven domain).
pub(crate) struct Node {
    pub(crate) kind: NodeKind,
    pub(crate) domain: ClockDomain,
    /// Bresenham phase accumulator for `FixedFrequency`/`OutputSample` domains.
    pub(crate) phase_acc: f64,
    /// Last input-generation this node evaluated, for the `EventOnly` domain.
    pub(crate) last_gen: u64,
}

impl Node {
    pub(crate) fn new(kind: NodeKind, domain: ClockDomain) -> Self {
        Self {
            kind,
            domain,
            phase_acc: 0.0,
            last_gen: 0,
        }
    }
}

/// The v1 primitive set. Common primitives are inline enum variants dispatched
/// by `match`; circuit-specific behavior goes through [`NodeKind::Custom`].
pub(crate) enum NodeKind {
    // --- Inputs (value set by the board via the wrapper) ---
    /// A 0/1 logic line. `inverted` flips it on read.
    LogicInput { value: f64, inverted: bool },
    /// A scalar data line. Emits `value * scale`.
    DataInput { value: f64, scale: f64 },
    /// A one-shot pulse. Emits `1.0` for a single evaluation, then `0.0`.
    PulseInput { pending: bool },
    /// A sample stream pushed in from another device (chip/DAC output).
    ExternalSource { value: f64 },

    // --- Sources ---
    /// Square wave at a fixed frequency (Hz).
    FixedSquare { freq: f64, phase: f64 },
    /// Square wave whose frequency (Hz) comes from another node.
    VariableSquare { freq_src: NodeId, phase: f64 },
    /// Triangle wave at a fixed frequency (Hz).
    FixedTriangle { freq: f64, phase: f64 },
    /// Triangle wave whose frequency (Hz) comes from another node.
    VariableTriangle { freq_src: NodeId, phase: f64 },
    /// Linear-feedback-shift-register noise, internally clocked at `freq` (Hz).
    LfsrNoise {
        lfsr: u32,
        seed: u32,
        tap_a: u8,
        tap_b: u8,
        width: u8,
        freq: f64,
        clock_acc: f64,
    },
    /// A fixed value.
    Constant { value: f64 },

    // --- Math / routing ---
    /// Rising-edge detector: `1.0` on the step where `src` increases, else `0.0`.
    EdgeDetector { src: NodeId, last: f64 },
    /// `src * gain`.
    Gain { src: NodeId, gain: f64 },
    /// Sum of all `srcs`.
    Add { srcs: Vec<NodeId> },
    /// `a * b`.
    Multiply { a: NodeId, b: NodeId },
    /// `src` clamped to `[lo, hi]`.
    Clamp { src: NodeId, lo: f64, hi: f64 },

    // --- Analog approximations ---
    /// One-pole RC low-pass tracking `src`, time constant `tau` (seconds).
    RcLowPass { src: NodeId, tau: f64, y: f64 },
    /// One-pole RC high-pass / coupling cap: passes transients, blocks DC.
    RcHighPass {
        src: NodeId,
        tau: f64,
        x_prev: f64,
        y: f64,
    },
    /// Asymmetric RC envelope charging toward `src` with separate rise/fall
    /// time constants (seconds) — capacitor charge/discharge behavior.
    RcEnvelope {
        src: NodeId,
        tau_charge: f64,
        tau_discharge: f64,
        v: f64,
    },
    /// Chamberlin state-variable 2nd-order filter (low/band/high-pass).
    SecondOrder {
        src: NodeId,
        mode: FilterMode,
        f0: f64,
        q: f64,
        low: f64,
        band: f64,
    },
    /// Passive resistor (weighted-average) mixer. `srcs` holds `(node,
    /// conductance)`; `total_g` is the precomputed denominator including any load.
    ResistorMixer {
        srcs: Vec<(NodeId, f64)>,
        total_g: f64,
    },
    /// Diode-OR mixer: the highest input wins, less that diode's forward drop.
    /// `srcs` holds `(node, drop)` so branches with different numbers of
    /// junctions in series can carry their own drop. The output cannot go
    /// below the reference — a reverse-biased diode conducts nothing.
    DiodeMixer { srcs: Vec<(NodeId, f64)> },
    /// DAC / resistor ladder: sums per-bit `weights` for each set bit of the
    /// integer code carried by `src`.
    DacLadder { src: NodeId, weights: Vec<f64> },

    // --- NE555 / op-amp analog primitives (ports of MAME's discrete core) ---
    /// NE555 astable oscillator (port of MAME `dsd_555_astable`). Charges a cap
    /// through `R1+R2` and discharges through `R2`, toggling a flip-flop at the
    /// 1/3·Vcc trigger and 2/3·Vcc threshold (or a modulating control voltage).
    /// The sub-sample threshold-crossing loop is dropped; faithful at high
    /// `sim_rate` (see [`DiscreteCircuitBuilder::ne555_astable`]).
    Ne555Astable {
        /// Optional control-voltage source; when present it sets the comparator
        /// threshold (and trigger = threshold/2) per step, modulating frequency.
        cv_src: Option<NodeId>,
        /// `1 - exp(-dt/((R1+R2)·C))`, the per-step charge fraction.
        exp_charge: f64,
        /// `1 - exp(-dt/(R2·C))`, the per-step discharge fraction.
        exp_discharge: f64,
        /// Voltage the cap charges toward (≈ Vcc).
        v_charge: f64,
        /// Threshold/trigger used when `cv_src` is `None` (2/3·Vcc, 1/3·Vcc).
        threshold_fixed: f64,
        trigger_fixed: f64,
        /// Square-wave high level (the MAME desc's `v_out_high`, e.g. Vcc − 1.2 V
        /// for the default, or a circuit-specific value such as 4.5 V).
        out_high: f64,
        output: Output555,
        cap_v: f64,
        flip_flop: bool,
    },
    /// NE555 constant-current VCO, simple type (port of `dsd_555_cc`, the
    /// no-RDIS/RGND/RBIAS case). A transistor current source charges `C` until
    /// 2/3·Vcc, then the cap discharges to 1/3·Vcc; output is the cap voltage.
    Ne555Cc {
        /// Control-voltage source (the current-setting input voltage).
        vin_src: NodeId,
        /// Charge resistor (ohms) and cap (farads) setting the current ramp.
        r: f64,
        c: f64,
        /// Constant-current source supply and its transistor junction drop.
        v_cc_source: f64,
        junction: f64,
        /// 2/3·Vcc threshold and 1/3·Vcc trigger.
        threshold: f64,
        trigger: f64,
        /// Square-wave high level (Vcc − 1.2 V).
        out_high: f64,
        output: Output555,
        cap_v: f64,
        flip_flop: bool,
    },
    /// Op-amp multiple-feedback band-pass (port of `dst_op_amp_filt`
    /// `IS_BAND_PASS_1M`): a biquad whose coefficients come from the op-amp's
    /// R/C values via a pre-warped bilinear transform, precomputed in the
    /// builder. Distinct from the Chamberlin [`NodeKind::SecondOrder`].
    OpAmpBandPass {
        src: NodeId,
        a1: f64,
        a2: f64,
        b0: f64,
        b2: f64,
        /// Input scale from `src` to the op-amp summing node (`rTotal/r_in[0]`).
        in_gain: f64,
        v_ref: f64,
        /// Output clamped to the op-amp rails `[clip_lo, clip_hi]` (MAME clips to
        /// `vN .. vP − 1.5`), which preserves overdrive distortion and bounds
        /// the power-on transient.
        clip_lo: f64,
        clip_hi: f64,
        x1: f64,
        x2: f64,
        y1: f64,
        y2: f64,
    },
    /// Gated diode + R//C discharge (port of `dst_rcdisc5`): a diode (0.7 V
    /// drop) feeds an R//C; while `enable` is high the cap tracks the input
    /// upward instantly and decays with `τ = R·C`, else it holds and outputs 0.
    RcDisc5 {
        in_src: NodeId,
        enable_src: NodeId,
        /// `1 - exp(-dt/(R·C))`, the per-step discharge fraction.
        charge_exp: f64,
        cap_v: f64,
    },
    /// Logic-triggered RC discharge, gated and modulated by a second input
    /// (port of `dst_rcdisc_mod`).
    ///
    /// A capacitor charges toward the supply through one resistor network while
    /// `trigger` is released and toward ground while it is asserted; the output
    /// is the voltage still across the charging resistor, so it is a decaying
    /// envelope rather than a level. `modulator` does two things at once: it
    /// switches a second resistor in and out, changing the decay rate, and it
    /// chops the output to zero whenever it is high.
    ///
    /// That chopping is what makes this different from multiplying an envelope
    /// by an oscillator. The output is a train of one-sided pulses — present
    /// only while the modulator is low — which carries far more low-frequency
    /// energy than the symmetric product of the same two signals.
    ///
    /// With `modulator` tied low it degenerates into a fixed-width pulse from
    /// the trigger edge, which is how a board conditions a latch write into a
    /// trigger of its own choosing.
    RcDiscModulated {
        trigger_src: NodeId,
        modulator_src: NodeId,
        v_supply: f64,
        /// Per-step charge fractions, indexed by `(modulator << 1) | trigger`.
        exp_high: [f64; 4],
        /// Divider ratios deciding when the diode clamp conducts, same index.
        vd_gain: [f64; 4],
        /// Per-step fractions and output divider while clamped, by trigger only.
        exp_low: [f64; 2],
        gain: [f64; 2],
        v_cap: f64,
    },
    /// CMOS inverter relaxation oscillator: a ring of inverters with a timing
    /// resistor from one output back to the input and a capacitor from another.
    ///
    /// The period is a fixed multiple of `R·C`, but the multiple depends on
    /// where the inverter chain actually switches, which is *not* at its
    /// datasheet thresholds and not at mid-supply. Modelling the gate's transfer
    /// curve is what makes the period fall out of component values; assuming an
    /// ideal threshold instead gets it wrong by ~20 %, and assuming the
    /// datasheet thresholds act as hysteresis gets it wrong by a factor of two
    /// in both directions.
    InverterOsc {
        /// Three inverters (resistor from the third, capacitor from the second)
        /// rather than two (resistor from the first, capacitor from the second).
        three_stage: bool,
        v_supply: f64,
        /// Input protection clamp: the input node is held to
        /// `[-clamp, v_supply + clamp]`, and the timing capacitor swings well
        /// past both, so this is part of the timing rather than a detail.
        clamp: f64,
        /// Transfer curve `v_supply · exp(-a·(x/v_supply)^b)`, solved at
        /// construction so it passes through both datasheet threshold points.
        tf_a: f64,
        tf_b: f64,
        /// Per-step charge fraction free-running, and while the clamp conducts
        /// and the bias resistor shares the current.
        exp_free: f64,
        exp_clamped: f64,
        /// `r_bias / (r_bias + r)`: how the target divides while clamped.
        ratio: f64,
        v_cap: f64,
        v_mid_prev: f64,
    },
    /// Binary counter clocked by another node's rising edges, output taken from
    /// its top bit — a divide-by-`divisor` square with even duty.
    ///
    /// Models a counter IC wired as a divider, which is how a board turns a fast
    /// source into a slow one. Note the clock need not be periodic: driven by a
    /// noise source this divides the *edges*, producing a square whose period
    /// varies with the source's run lengths. That is a different signal from the
    /// same noise low-passed to the same average frequency — it has a
    /// fundamental where the filtered version only has a spectral tilt.
    EdgeDivider {
        clock_src: NodeId,
        /// Rising edges per output period. Halved to get the toggle interval.
        divisor: u32,
        count: u32,
        level: bool,
        /// Previous clock value, for edge detection.
        last: f64,
    },
    /// Emitter follower charging a capacitor (port of `dst_rcintegrate`, type 1).
    /// The base is `src`; the emitter sits on `r_e` into `c`, with `r_load` to
    /// ground. While the base is high enough the transistor conducts and the cap
    /// charges toward `src − v_be` with `τ = r_e·c`; once the base falls below
    /// the emitter the transistor cuts off and the cap drains through
    /// `r_e + r_load` toward ground.
    ///
    /// The asymmetry is the point, and it is not a low-pass: charge and
    /// discharge have different time constants *and different targets*. A
    /// symmetric RC in its place settles on the input's mean, where this one
    /// tracks its peaks and sags between them.
    RcIntegrate {
        src: NodeId,
        v_be: f64,
        /// Fraction of the gap closed per step while conducting, `1 - exp(-dt/(r_e·c))`.
        charge_exp: f64,
        /// Fraction closed per step while cut off, `1 - exp(-dt/((r_e+r_load)·c))`.
        discharge_exp: f64,
        cap_v: f64,
    },

    // --- Escape hatch ---
    /// Circuit-specific behavior. The only dynamically dispatched variant.
    Custom {
        inputs: Vec<NodeId>,
        comp: Box<dyn CustomComponent>,
        /// Reused per-eval input buffer (runtime only, not serialized).
        scratch: Vec<f64>,
    },
}

impl NodeKind {
    /// Append the node indices this kind reads from. Used to build the
    /// evaluation order; sources and inputs have no graph dependencies.
    pub(crate) fn deps(&self, out: &mut Vec<usize>) {
        match self {
            NodeKind::EdgeDetector { src, .. }
            | NodeKind::Gain { src, .. }
            | NodeKind::Clamp { src, .. }
            | NodeKind::RcLowPass { src, .. }
            | NodeKind::RcHighPass { src, .. }
            | NodeKind::RcEnvelope { src, .. }
            | NodeKind::SecondOrder { src, .. }
            | NodeKind::DacLadder { src, .. }
            | NodeKind::RcIntegrate { src, .. }
            | NodeKind::OpAmpBandPass { src, .. } => out.push(src.index()),
            NodeKind::Ne555Cc { vin_src, .. } => out.push(vin_src.index()),
            NodeKind::Ne555Astable {
                cv_src: Some(cv), ..
            } => out.push(cv.index()),
            NodeKind::RcDisc5 {
                in_src, enable_src, ..
            } => {
                out.push(in_src.index());
                out.push(enable_src.index());
            }
            NodeKind::VariableSquare { freq_src, .. }
            | NodeKind::VariableTriangle { freq_src, .. } => out.push(freq_src.index()),
            NodeKind::Multiply { a, b } => {
                out.push(a.index());
                out.push(b.index());
            }
            NodeKind::Add { srcs } => out.extend(srcs.iter().map(|s| s.index())),
            NodeKind::DiodeMixer { srcs } => out.extend(srcs.iter().map(|(s, _)| s.index())),
            NodeKind::EdgeDivider { clock_src, .. } => out.push(clock_src.index()),
            NodeKind::RcDiscModulated {
                trigger_src,
                modulator_src,
                ..
            } => {
                out.push(trigger_src.index());
                out.push(modulator_src.index());
            }
            NodeKind::ResistorMixer { srcs, .. } => out.extend(srcs.iter().map(|(s, _)| s.index())),
            NodeKind::Custom { inputs, .. } => out.extend(inputs.iter().map(|s| s.index())),
            _ => {}
        }
    }

    /// Compute this node's output for one simulation step. Reads inputs from the
    /// shared `values` slice (already holding this step's value for forward
    /// edges and last step's for back-edges) and advances any internal state.
    pub(crate) fn eval(&mut self, values: &[f64], dt: f64) -> f64 {
        match self {
            NodeKind::LogicInput { value, inverted } => {
                if *inverted {
                    1.0 - *value
                } else {
                    *value
                }
            }
            NodeKind::DataInput { value, scale } => *value * *scale,
            NodeKind::PulseInput { pending } => {
                if *pending {
                    *pending = false;
                    1.0
                } else {
                    0.0
                }
            }
            NodeKind::ExternalSource { value } => *value,
            NodeKind::Constant { value } => *value,

            NodeKind::FixedSquare { freq, phase } => {
                *phase += *freq * dt;
                *phase -= phase.floor();
                if *phase < 0.5 { 1.0 } else { -1.0 }
            }
            NodeKind::VariableSquare { freq_src, phase } => {
                let freq = values[freq_src.index()];
                *phase += freq * dt;
                *phase -= phase.floor();
                if *phase < 0.5 { 1.0 } else { -1.0 }
            }
            NodeKind::FixedTriangle { freq, phase } => {
                *phase += *freq * dt;
                *phase -= phase.floor();
                1.0 - 4.0 * (*phase - 0.5).abs()
            }
            NodeKind::VariableTriangle { freq_src, phase } => {
                let freq = values[freq_src.index()];
                *phase += freq * dt;
                *phase -= phase.floor();
                1.0 - 4.0 * (*phase - 0.5).abs()
            }
            NodeKind::LfsrNoise {
                lfsr,
                tap_a,
                tap_b,
                width,
                freq,
                clock_acc,
                ..
            } => {
                *clock_acc += *freq * dt;
                while *clock_acc >= 1.0 {
                    *clock_acc -= 1.0;
                    let bit = ((*lfsr >> *tap_a) ^ (*lfsr >> *tap_b)) & 1;
                    *lfsr = (*lfsr >> 1) | (bit << (*width - 1));
                }
                if *lfsr & 1 != 0 { 1.0 } else { -1.0 }
            }

            NodeKind::EdgeDetector { src, last } => {
                let cur = values[src.index()];
                let out = if cur > *last { 1.0 } else { 0.0 };
                *last = cur;
                out
            }
            NodeKind::Gain { src, gain } => values[src.index()] * *gain,
            NodeKind::Add { srcs } => srcs.iter().map(|s| values[s.index()]).sum(),
            NodeKind::Multiply { a, b } => values[a.index()] * values[b.index()],
            NodeKind::Clamp { src, lo, hi } => values[src.index()].clamp(*lo, *hi),

            NodeKind::RcLowPass { src, tau, y } => {
                let x = values[src.index()];
                let alpha = dt / (*tau + dt);
                *y += alpha * (x - *y);
                *y
            }
            NodeKind::RcHighPass {
                src,
                tau,
                x_prev,
                y,
            } => {
                let x = values[src.index()];
                let alpha = *tau / (*tau + dt);
                *y = alpha * (*y + x - *x_prev);
                *x_prev = x;
                *y
            }
            NodeKind::RcEnvelope {
                src,
                tau_charge,
                tau_discharge,
                v,
            } => {
                let target = values[src.index()];
                let tau = if target > *v {
                    *tau_charge
                } else {
                    *tau_discharge
                };
                let alpha = if tau <= 0.0 { 1.0 } else { dt / (tau + dt) };
                *v += alpha * (target - *v);
                *v
            }
            NodeKind::SecondOrder {
                src,
                mode,
                f0,
                q,
                low,
                band,
            } => {
                // Chamberlin state-variable filter; stable for f0 < sim_rate/6.
                let input = values[src.index()];
                let f = 2.0 * (std::f64::consts::PI * *f0 * dt).sin();
                let q1 = 1.0 / *q;
                *low += f * *band;
                let high = input - *low - q1 * *band;
                *band += f * high;
                match mode {
                    FilterMode::LowPass => *low,
                    FilterMode::BandPass => *band,
                    FilterMode::HighPass => high,
                }
            }
            NodeKind::ResistorMixer { srcs, total_g } => {
                if *total_g == 0.0 {
                    0.0
                } else {
                    let sum: f64 = srcs.iter().map(|(s, g)| values[s.index()] * g).sum();
                    sum / *total_g
                }
            }
            NodeKind::DiodeMixer { srcs } => srcs
                .iter()
                .map(|(s, drop)| values[s.index()] - *drop)
                .fold(0.0, f64::max),
            NodeKind::DacLadder { src, weights } => {
                let code = values[src.index()].round().max(0.0) as u32;
                weights
                    .iter()
                    .enumerate()
                    .filter(|(b, _)| (code >> b) & 1 == 1)
                    .map(|(_, w)| *w)
                    .sum()
            }

            NodeKind::Ne555Astable {
                cv_src,
                exp_charge,
                exp_discharge,
                v_charge,
                threshold_fixed,
                trigger_fixed,
                out_high,
                output,
                cap_v,
                flip_flop,
            } => {
                // Thresholds: a control voltage sets threshold = CV and
                // trigger = CV/2; otherwise the fixed 2/3·Vcc / 1/3·Vcc.
                let (threshold, trigger) = match cv_src {
                    Some(cv) => {
                        let cv = values[cv.index()];
                        // A CV under 0.25 V drives the 555 far out of range;
                        // MAME ignores it and holds the prior output.
                        if cv < 0.25 {
                            return match output {
                                Output555::Square => {
                                    if *flip_flop {
                                        *out_high
                                    } else {
                                        0.0
                                    }
                                }
                                Output555::Capacitor => *cap_v,
                            };
                        }
                        // The new thresholds may already be crossed by the cap.
                        if *cap_v >= cv {
                            *flip_flop = false;
                        } else if *cap_v <= cv / 2.0 {
                            *flip_flop = true;
                        }
                        (cv, cv / 2.0)
                    }
                    None => (*threshold_fixed, *trigger_fixed),
                };
                if *flip_flop {
                    // Charging through R1+R2 toward v_charge.
                    *cap_v += (*v_charge - *cap_v) * *exp_charge;
                    if *cap_v >= threshold {
                        *cap_v = threshold;
                        *flip_flop = false;
                    }
                } else {
                    // Discharging through R2 toward 0.
                    *cap_v -= *cap_v * *exp_discharge;
                    if *cap_v <= trigger {
                        *cap_v = trigger;
                        *flip_flop = true;
                    }
                }
                match output {
                    Output555::Square => {
                        if *flip_flop {
                            *out_high
                        } else {
                            0.0
                        }
                    }
                    Output555::Capacitor => *cap_v,
                }
            }
            NodeKind::Ne555Cc {
                vin_src,
                r,
                c,
                v_cc_source,
                junction,
                threshold,
                trigger,
                out_high,
                output,
                cap_v,
                flip_flop,
            } => {
                // The current source charges the cap; vin + junction caps the
                // voltage it can reach. i = (v_cc_source - (vin+junction))/R.
                let v_charge_limit = values[vin_src.index()] + *junction;
                let i = ((*v_cc_source - v_charge_limit) / *r).max(0.0);
                if *flip_flop {
                    // Constant-current charge: dv = i·dt/C, clamped to the limit.
                    *cap_v = (*cap_v + i * dt / *c).min(v_charge_limit);
                    if *cap_v >= *threshold {
                        *cap_v = *threshold;
                        *flip_flop = false;
                    }
                } else {
                    // No discharge resistor: immediate drop to the trigger.
                    *cap_v = *trigger;
                    *flip_flop = true;
                }
                match output {
                    Output555::Square => {
                        if *flip_flop {
                            *out_high
                        } else {
                            0.0
                        }
                    }
                    Output555::Capacitor => *cap_v,
                }
            }
            NodeKind::OpAmpBandPass {
                src,
                a1,
                a2,
                b0,
                b2,
                in_gain,
                v_ref,
                clip_lo,
                clip_hi,
                x1,
                x2,
                y1,
                y2,
            } => {
                // Op-amp summing node, relative to the reference rail.
                let v = *in_gain * values[src.index()] - *v_ref;
                let mut out = -*a1 * *y1 - *a2 * *y2 + *b0 * v + *b2 * *x2 + *v_ref;
                *x2 = *x1;
                *x1 = v;
                *y2 = *y1;
                // Clip to the op-amp rails, then feed back the clipped output.
                out = out.clamp(*clip_lo, *clip_hi);
                *y1 = out - *v_ref;
                out
            }
            NodeKind::RcDisc5 {
                in_src,
                enable_src,
                charge_exp,
                cap_v,
            } => {
                let u = (values[in_src.index()] - 0.7).max(0.0);
                let mut diff = u - *cap_v;
                if values[enable_src.index()] != 0.0 {
                    // Tracks the input up instantly, decays down with τ = R·C.
                    if diff < 0.0 {
                        diff *= *charge_exp;
                    }
                    *cap_v += diff;
                    *cap_v
                } else {
                    // Gate released: hold the higher of cap/input, output muted.
                    if diff > 0.0 {
                        *cap_v = u;
                    }
                    0.0
                }
            }
            NodeKind::RcDiscModulated {
                trigger_src,
                modulator_src,
                v_supply,
                exp_high,
                vd_gain,
                exp_low,
                gain,
                v_cap,
            } => {
                let trig = usize::from(values[trigger_src.index()] > 0.5);
                let modu = usize::from(values[modulator_src.index()] > 0.6);
                let state = (modu << 1) | trig;
                // Asserted pulls the network to ground; released lets it charge.
                let u = if trig == 1 { 0.0 } else { *v_supply };
                let diff = u - *v_cap;
                if diff * vd_gain[state] < -0.6 {
                    // The clamp diode conducts, holding the output at its drop.
                    let mut d = u + 0.6 - *v_cap;
                    d -= d * exp_low[trig];
                    *v_cap += d;
                    if modu == 1 { 0.0 } else { -0.6 }
                } else {
                    let mut d = diff;
                    d -= d * exp_high[state];
                    *v_cap += d;
                    if modu == 1 {
                        0.0
                    } else {
                        (u - *v_cap) * gain[trig]
                    }
                }
            }
            NodeKind::InverterOsc {
                three_stage,
                v_supply,
                clamp,
                tf_a,
                tf_b,
                exp_free,
                exp_clamped,
                ratio,
                v_cap,
                v_mid_prev,
            } => {
                let vb = *v_supply;
                let tf = |x: f64| {
                    if x <= 0.0 {
                        vb
                    } else {
                        vb * (-*tf_a * (x / vb).powf(*tf_b)).exp()
                    }
                };
                // The capacitor sits between the mid node and the input, so the
                // input rides on whatever the mid node did last step.
                let v_in = *v_cap + *v_mid_prev;
                let (v_out, v_mid) = if *three_stage {
                    let a = tf(v_in);
                    let mid = tf(a);
                    (tf(mid), mid)
                } else {
                    let out = tf(v_in);
                    (out, tf(out))
                };
                if v_in < -*clamp || v_in > vb + *clamp {
                    // Protection diodes conducting: the bias resistor now shares
                    // the charging current, so both the rate and the target move.
                    let v_in_c = v_in.clamp(-*clamp, vb + *clamp);
                    let target = v_out * *ratio + v_in_c * (1.0 - *ratio) - v_mid;
                    *v_cap += (target - *v_cap) * *exp_clamped;
                } else {
                    *v_cap += ((v_out - v_mid) - *v_cap) * *exp_free;
                }
                *v_mid_prev = v_mid;
                v_out
            }
            NodeKind::EdgeDivider {
                clock_src,
                divisor,
                count,
                level,
                last,
            } => {
                let cur = values[clock_src.index()];
                // Rising edge through zero: the clock swings either 0/1 or ±1,
                // and both cross zero exactly once per period.
                if cur > 0.0 && *last <= 0.0 {
                    *count += 1;
                    if *count >= (*divisor).max(2) / 2 {
                        *count = 0;
                        *level = !*level;
                    }
                }
                *last = cur;
                if *level { 1.0 } else { 0.0 }
            }
            NodeKind::RcIntegrate {
                src,
                v_be,
                charge_exp,
                discharge_exp,
                cap_v,
            } => {
                let emitter = values[src.index()] - *v_be;
                if emitter > *cap_v {
                    // Conducting: the emitter drives the cap through r_e.
                    *cap_v += (emitter - *cap_v) * *charge_exp;
                } else {
                    // Cut off: nothing holds the cap up, so it drains to ground
                    // through r_e + r_load — not back down to the input.
                    *cap_v -= *cap_v * *discharge_exp;
                }
                *cap_v
            }

            NodeKind::Custom {
                inputs,
                comp,
                scratch,
            } => {
                scratch.clear();
                scratch.extend(inputs.iter().map(|s| values[s.index()]));
                comp.step(scratch, dt)
            }
        }
    }

    /// Restore power-on runtime state, preserving static configuration.
    pub(crate) fn reset_runtime(&mut self) {
        match self {
            NodeKind::LogicInput { value, .. }
            | NodeKind::DataInput { value, .. }
            | NodeKind::ExternalSource { value } => *value = 0.0,
            NodeKind::PulseInput { pending } => *pending = false,
            NodeKind::FixedSquare { phase, .. }
            | NodeKind::VariableSquare { phase, .. }
            | NodeKind::FixedTriangle { phase, .. }
            | NodeKind::VariableTriangle { phase, .. } => *phase = 0.0,
            NodeKind::LfsrNoise {
                lfsr,
                seed,
                clock_acc,
                ..
            } => {
                *lfsr = *seed;
                *clock_acc = 0.0;
            }
            NodeKind::EdgeDetector { last, .. } => *last = 0.0,
            NodeKind::RcLowPass { y, .. } => *y = 0.0,
            NodeKind::RcHighPass { x_prev, y, .. } => {
                *x_prev = 0.0;
                *y = 0.0;
            }
            NodeKind::RcEnvelope { v, .. } => *v = 0.0,
            NodeKind::SecondOrder { low, band, .. } => {
                *low = 0.0;
                *band = 0.0;
            }
            NodeKind::Ne555Astable {
                cap_v, flip_flop, ..
            }
            | NodeKind::Ne555Cc {
                cap_v, flip_flop, ..
            } => {
                *cap_v = 0.0;
                *flip_flop = true;
            }
            NodeKind::OpAmpBandPass {
                v_ref,
                x1,
                x2,
                y1,
                y2,
                ..
            } => {
                // Start in the steady state for zero input (v = −v_ref), so the
                // filter doesn't ring on power-on before any signal arrives.
                *x1 = -*v_ref;
                *x2 = -*v_ref;
                *y1 = 0.0;
                *y2 = 0.0;
            }
            NodeKind::RcDisc5 { cap_v, .. } | NodeKind::RcIntegrate { cap_v, .. } => *cap_v = 0.0,
            NodeKind::EdgeDivider {
                count, level, last, ..
            } => {
                *count = 0;
                *level = false;
                *last = 0.0;
            }
            NodeKind::InverterOsc {
                v_cap, v_mid_prev, ..
            } => {
                *v_cap = 0.0;
                *v_mid_prev = 0.0;
            }
            NodeKind::RcDiscModulated { v_cap, .. } => *v_cap = 0.0,
            NodeKind::Custom { comp, .. } => comp.reset(),
            NodeKind::Constant { .. }
            | NodeKind::Gain { .. }
            | NodeKind::Add { .. }
            | NodeKind::Multiply { .. }
            | NodeKind::Clamp { .. }
            | NodeKind::ResistorMixer { .. }
            | NodeKind::DiodeMixer { .. }
            | NodeKind::DacLadder { .. } => {}
        }
    }

    /// Serialize the mutable runtime state of this node (not its topology).
    pub(crate) fn save_runtime(&self, w: &mut StateWriter) {
        match self {
            NodeKind::LogicInput { value, .. }
            | NodeKind::DataInput { value, .. }
            | NodeKind::ExternalSource { value } => w.write_f64_le(*value),
            NodeKind::PulseInput { pending } => w.write_bool(*pending),
            NodeKind::FixedSquare { phase, .. }
            | NodeKind::VariableSquare { phase, .. }
            | NodeKind::FixedTriangle { phase, .. }
            | NodeKind::VariableTriangle { phase, .. } => w.write_f64_le(*phase),
            NodeKind::LfsrNoise {
                lfsr, clock_acc, ..
            } => {
                w.write_u32_le(*lfsr);
                w.write_f64_le(*clock_acc);
            }
            NodeKind::EdgeDetector { last, .. } => w.write_f64_le(*last),
            NodeKind::RcLowPass { y, .. } => w.write_f64_le(*y),
            NodeKind::RcHighPass { x_prev, y, .. } => {
                w.write_f64_le(*x_prev);
                w.write_f64_le(*y);
            }
            NodeKind::RcEnvelope { v, .. } => w.write_f64_le(*v),
            NodeKind::SecondOrder { low, band, .. } => {
                w.write_f64_le(*low);
                w.write_f64_le(*band);
            }
            NodeKind::Ne555Astable {
                cap_v, flip_flop, ..
            }
            | NodeKind::Ne555Cc {
                cap_v, flip_flop, ..
            } => {
                w.write_f64_le(*cap_v);
                w.write_bool(*flip_flop);
            }
            NodeKind::OpAmpBandPass { x1, x2, y1, y2, .. } => {
                w.write_f64_le(*x1);
                w.write_f64_le(*x2);
                w.write_f64_le(*y1);
                w.write_f64_le(*y2);
            }
            NodeKind::RcDisc5 { cap_v, .. } | NodeKind::RcIntegrate { cap_v, .. } => {
                w.write_f64_le(*cap_v)
            }
            NodeKind::EdgeDivider {
                count, level, last, ..
            } => {
                w.write_u32_le(*count);
                w.write_bool(*level);
                w.write_f64_le(*last);
            }
            NodeKind::InverterOsc {
                v_cap, v_mid_prev, ..
            } => {
                w.write_f64_le(*v_cap);
                w.write_f64_le(*v_mid_prev);
            }
            NodeKind::RcDiscModulated { v_cap, .. } => w.write_f64_le(*v_cap),
            NodeKind::Custom { comp, .. } => comp.save_state(w),
            NodeKind::Constant { .. }
            | NodeKind::Gain { .. }
            | NodeKind::Add { .. }
            | NodeKind::Multiply { .. }
            | NodeKind::Clamp { .. }
            | NodeKind::ResistorMixer { .. }
            | NodeKind::DiodeMixer { .. }
            | NodeKind::DacLadder { .. } => {}
        }
    }

    /// Restore the mutable runtime state written by [`save_runtime`].
    pub(crate) fn load_runtime(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        match self {
            NodeKind::LogicInput { value, .. }
            | NodeKind::DataInput { value, .. }
            | NodeKind::ExternalSource { value } => *value = r.read_f64_le()?,
            NodeKind::PulseInput { pending } => *pending = r.read_bool()?,
            NodeKind::FixedSquare { phase, .. }
            | NodeKind::VariableSquare { phase, .. }
            | NodeKind::FixedTriangle { phase, .. }
            | NodeKind::VariableTriangle { phase, .. } => *phase = r.read_f64_le()?,
            NodeKind::LfsrNoise {
                lfsr, clock_acc, ..
            } => {
                *lfsr = r.read_u32_le()?;
                *clock_acc = r.read_f64_le()?;
            }
            NodeKind::EdgeDetector { last, .. } => *last = r.read_f64_le()?,
            NodeKind::RcLowPass { y, .. } => *y = r.read_f64_le()?,
            NodeKind::RcHighPass { x_prev, y, .. } => {
                *x_prev = r.read_f64_le()?;
                *y = r.read_f64_le()?;
            }
            NodeKind::RcEnvelope { v, .. } => *v = r.read_f64_le()?,
            NodeKind::SecondOrder { low, band, .. } => {
                *low = r.read_f64_le()?;
                *band = r.read_f64_le()?;
            }
            NodeKind::Ne555Astable {
                cap_v, flip_flop, ..
            }
            | NodeKind::Ne555Cc {
                cap_v, flip_flop, ..
            } => {
                *cap_v = r.read_f64_le()?;
                *flip_flop = r.read_bool()?;
            }
            NodeKind::OpAmpBandPass { x1, x2, y1, y2, .. } => {
                *x1 = r.read_f64_le()?;
                *x2 = r.read_f64_le()?;
                *y1 = r.read_f64_le()?;
                *y2 = r.read_f64_le()?;
            }
            NodeKind::RcDisc5 { cap_v, .. } | NodeKind::RcIntegrate { cap_v, .. } => {
                *cap_v = r.read_f64_le()?
            }
            NodeKind::EdgeDivider {
                count, level, last, ..
            } => {
                *count = r.read_u32_le()?;
                *level = r.read_bool()?;
                *last = r.read_f64_le()?;
            }
            NodeKind::InverterOsc {
                v_cap, v_mid_prev, ..
            } => {
                *v_cap = r.read_f64_le()?;
                *v_mid_prev = r.read_f64_le()?;
            }
            NodeKind::RcDiscModulated { v_cap, .. } => *v_cap = r.read_f64_le()?,
            NodeKind::Custom { comp, .. } => comp.load_state(r)?,
            NodeKind::Constant { .. }
            | NodeKind::Gain { .. }
            | NodeKind::Add { .. }
            | NodeKind::Multiply { .. }
            | NodeKind::Clamp { .. }
            | NodeKind::ResistorMixer { .. }
            | NodeKind::DiodeMixer { .. }
            | NodeKind::DacLadder { .. } => {}
        }
        Ok(())
    }
}
