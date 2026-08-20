//! Reusable runtime for discrete / board-level analog sound circuits.
//!
//! A board describes its sound circuit once, in typed Rust, with a
//! [`DiscreteCircuitBuilder`]: it allocates named inputs and primitive nodes and
//! wires them by typed handle. [`DiscreteCircuitBuilder::build`] freezes the
//! topology into a [`DiscreteCircuit`] that the board drives with
//! [`DiscreteCircuit::tick`] and drains with [`DiscreteCircuit::fill_audio`].
//!
//! # Evaluation model
//!
//! Nodes live in a contiguous `Vec`, each owning one slot in a parallel `values`
//! array. At build time the graph is topologically sorted into an `eval_order`:
//! a node always evaluates after the nodes it reads from. Each step iterates
//! `eval_order` once, reading inputs from `values` and writing the node's own
//! slot in place. Because of the order, a **forward edge** reads this step's
//! freshly computed value while a **back-edge** (an input that sorts *later*,
//! i.e. part of a feedback cycle) reads last step's value still held in its slot.
//! The same held-slot behavior gives cross-clock-domain sample-and-hold: a node
//! that is not due this step simply keeps its previous slot value.
//!
//! Cycles are detected during the sort (DFS); the edges that close a cycle are
//! left as back-edges rather than rejected, so feedback loops resolve with a
//! one-step delay.

mod derive;
mod node;

use node::{Node, NodeKind};

use crate::core::save_state::{SaveError, StateReader, StateWriter};

// ---------------------------------------------------------------------------
// Typed handles
// ---------------------------------------------------------------------------

/// Handle to a node in a circuit. Returned by builder methods and accepted as a
/// wiring input. Indexes both the node list and its value slot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NodeId(u16);

impl NodeId {
    pub(crate) fn index(self) -> usize {
        self.0 as usize
    }
}

/// Handle to a logic (0/1) input line. Set with [`DiscreteCircuit::set_logic`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LogicInputId(NodeId);
/// Handle to a scalar data input. Set with [`DiscreteCircuit::set_data`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DataInputId(NodeId);
/// Handle to a one-shot pulse input. Fired with [`DiscreteCircuit::pulse`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PulseInputId(NodeId);
/// Handle to an external sample-stream input. Fed with
/// [`DiscreteCircuit::set_external`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ExternalSourceId(NodeId);

macro_rules! into_node_id {
    ($($t:ty),*) => {$(
        impl From<$t> for NodeId {
            fn from(v: $t) -> NodeId { v.0 }
        }
    )*};
}
into_node_id!(LogicInputId, DataInputId, PulseInputId, ExternalSourceId);

// ---------------------------------------------------------------------------
// Clock domains, gain, LFSR spec, custom escape hatch
// ---------------------------------------------------------------------------

/// When a node re-evaluates. Nodes that are not due hold their previous output.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ClockDomain {
    /// Evaluate every simulation step (the base rate). The default.
    BoardCycle,
    /// Evaluate at a component-specific frequency in Hz.
    FixedFrequency(f64),
    /// Evaluate once per produced output sample.
    OutputSample,
    /// Evaluate only when an input changed since the last evaluation.
    EventOnly,
}

/// Output tap of a second-order filter.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum FilterMode {
    /// Low-pass.
    LowPass,
    /// Band-pass.
    BandPass,
    /// High-pass.
    HighPass,
}

/// Output tap of an NE555 oscillator primitive.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Output555 {
    /// The logic square wave (`out_high` while charging, 0 V while discharging).
    Square,
    /// The capacitor voltage (a ramp/triangle between the trigger and threshold).
    Capacitor,
}

/// Final output scaling applied before the signal enters the resampler.
#[derive(Clone, Copy, Debug)]
pub struct OutputGain(f64);

impl OutputGain {
    /// Unity gain.
    pub fn unity() -> Self {
        OutputGain(1.0)
    }
    /// Linear gain factor.
    pub fn linear(g: f64) -> Self {
        OutputGain(g)
    }
}

/// Which way an [`LfsrSpec`]'s register shifts — which is what its tap numbers
/// mean, and therefore which polynomial it runs.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum LfsrShift {
    /// Toward bit 0, the newest bit entering at the top. Convenient, but the tap
    /// numbers then count from the *oldest* bit, so they do not correspond to a
    /// schematic's.
    TowardZero,
    /// Toward the high end, the newest bit entering at bit 0 — how a chain of
    /// shift registers is actually wired, so bit *n* is the bit shifted in *n*
    /// steps ago and tap numbers mean what a schematic says they mean.
    TowardHigh,
}

/// Where an [`LfsrSpec`]'s output is taken from.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum LfsrOutput {
    /// Bit 0 of the register.
    RegisterBit,
    /// The feedback term itself, before it is shifted in. Real noise circuits
    /// often tap the XOR gate rather than the register.
    Feedback,
}

/// Configuration for a [`DiscreteCircuitBuilder::lfsr_noise`] generator.
///
/// The last three fields exist because "taps (10, 23)" is not by itself a
/// polynomial. Read off a schematic those numbers describe a register shifting
/// one way; implemented shifting the other way they describe a different
/// recurrence, and the failure mode is not an error but a *shortened cycle* —
/// which sounds like a repeating pattern rather than noise.
#[derive(Clone, Copy, Debug)]
pub struct LfsrSpec {
    /// Register width in bits (1..=32).
    pub width: u8,
    /// The two XOR tap bit positions.
    pub taps: (u8, u8),
    /// Non-zero seed loaded at reset.
    pub seed: u32,
    /// Shift direction, which decides what `taps` refers to.
    pub shift: LfsrShift,
    /// Invert the feedback before shifting it in. Some circuits do; it changes
    /// the sequence and the fixed point.
    pub invert_feedback: bool,
    /// Whether to emit a register bit or the feedback term.
    pub output: LfsrOutput,
}

/// A CMOS inverting gate, as its datasheet transfer characteristic.
///
/// The four voltages are what a 4000-series datasheet publishes for a given
/// supply, and they are enough to place the gate's transfer curve. They matter
/// because an inverter's switching point is not at mid-supply and not at the
/// quoted input thresholds — see [`inverter_osc`](DiscreteCircuitBuilder::inverter_osc).
#[derive(Clone, Copy, Debug)]
pub struct CmosInverter {
    /// Supply rail (volts).
    pub v_supply: f64,
    /// Output level driven low.
    pub v_out_low: f64,
    /// Output level driven high.
    pub v_out_high: f64,
    /// Input level at which a falling input has driven the output fully high.
    pub v_in_rise: f64,
    /// Input level at which a rising input has driven the output fully low.
    pub v_in_fall: f64,
    /// How far the input protection diodes let the input past either rail.
    pub input_clamp: f64,
}

impl LfsrSpec {
    /// The shape this framework used before the shift direction was explicit:
    /// shifting toward bit 0, feedback uninverted, output from bit 0.
    ///
    /// Kept so callers whose tap numbers were chosen against that behaviour keep
    /// it. It is NOT the arrangement a schematic describes — see
    /// [`LfsrShift`] — so prefer stating the real one for new circuits, and
    /// check the cycle length before migrating an existing one.
    pub fn toward_zero(width: u8, taps: (u8, u8), seed: u32) -> Self {
        Self {
            width,
            taps,
            seed,
            shift: LfsrShift::TowardZero,
            invert_feedback: false,
            output: LfsrOutput::RegisterBit,
        }
    }
}

impl CmosInverter {
    /// Typical unbuffered 4000-series inverter at the given supply: rails within
    /// 2 % of each, thresholds at 30 % and 70 % of supply, 0.1 V of input clamp.
    pub fn cd40xx(v_supply: f64) -> Self {
        Self {
            v_supply,
            v_out_low: v_supply * 0.02,
            v_out_high: v_supply * 0.98,
            v_in_rise: v_supply * 0.30,
            v_in_fall: v_supply * 0.70,
            input_clamp: 0.1,
        }
    }
}

/// Which arrangement of inverters an [`inverter_osc`](DiscreteCircuitBuilder::inverter_osc)
/// uses. They differ in more than gate count: which output drives the timing
/// resistor and which drives the capacitor is not the same in the two.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum InverterOsc {
    /// Two gates: the first's output drives the timing resistor, the second's
    /// the capacitor.
    TwoStage,
    /// Three gates: the third's output drives the timing resistor, the second's
    /// the capacitor.
    ThreeStage,
}

/// Escape hatch for circuit-specific behavior that does not justify a shared
/// primitive. Held by a `Custom` node — the only dynamically dispatched kind.
pub trait CustomComponent {
    /// Restore power-on state, preserving static configuration.
    fn reset(&mut self);
    /// Compute one output from the current input values and elapsed time.
    fn step(&mut self, inputs: &[f64], dt: f64) -> f64;
    /// Serialize mutable runtime state.
    fn save_state(&self, w: &mut StateWriter);
    /// Restore mutable runtime state.
    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError>;
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Builds an immutable circuit topology and allocates typed handles.
pub struct DiscreteCircuitBuilder {
    board_clock_hz: u64,
    output_sample_rate: u64,
    sim_rate: u64,
    nodes: Vec<Node>,
    names: Vec<String>,
    output: Option<(NodeId, OutputGain)>,
}

impl DiscreteCircuitBuilder {
    /// Start a circuit driven by a `board_clock_hz` board clock and producing
    /// `output_sample_rate` audio. The simulation runs at `output_sample_rate`
    /// by default; raise it with [`with_sim_rate`](Self::with_sim_rate).
    pub fn new(board_clock_hz: u64, output_sample_rate: u64) -> Self {
        Self {
            board_clock_hz,
            output_sample_rate,
            sim_rate: output_sample_rate,
            nodes: Vec::new(),
            names: Vec::new(),
            output: None,
        }
    }

    /// Override the internal simulation step rate (Hz). Must be high enough to
    /// represent the fastest node; the output is resampled down to the audio
    /// rate. Components that model analog state see `dt = 1 / sim_rate`.
    pub fn with_sim_rate(mut self, sim_rate: u64) -> Self {
        self.sim_rate = sim_rate;
        self
    }

    /// The handle the next created node will receive. Use this to wire a
    /// feedback edge: reference the id before creating the node that owns it
    /// (the self/forward reference becomes a back-edge with a one-step delay).
    pub fn next_id(&self) -> NodeId {
        NodeId(self.nodes.len() as u16)
    }

    fn push_node(&mut self, name: &str, kind: NodeKind, domain: ClockDomain) -> NodeId {
        let id = NodeId(self.nodes.len() as u16);
        self.nodes.push(Node::new(kind, domain));
        self.names.push(name.to_string());
        id
    }

    /// Allocate a 0/1 logic input line.
    pub fn logic_input(&mut self, name: &str) -> LogicInputId {
        LogicInputId(self.push_node(
            name,
            NodeKind::LogicInput {
                value: 0.0,
                inverted: false,
            },
            ClockDomain::BoardCycle,
        ))
    }

    /// Allocate an inverted 0/1 logic input line.
    pub fn inverted_logic_input(&mut self, name: &str) -> LogicInputId {
        LogicInputId(self.push_node(
            name,
            NodeKind::LogicInput {
                value: 0.0,
                inverted: true,
            },
            ClockDomain::BoardCycle,
        ))
    }

    /// Allocate a scalar data input emitting `value * scale`.
    pub fn data_input(&mut self, name: &str, scale: f64) -> DataInputId {
        DataInputId(self.push_node(
            name,
            NodeKind::DataInput { value: 0.0, scale },
            ClockDomain::BoardCycle,
        ))
    }

    /// Allocate a one-shot pulse input.
    pub fn pulse_input(&mut self, name: &str) -> PulseInputId {
        PulseInputId(self.push_node(
            name,
            NodeKind::PulseInput { pending: false },
            ClockDomain::BoardCycle,
        ))
    }

    /// Allocate an external sample-stream input (e.g. a chip or DAC output).
    pub fn external_source(&mut self, name: &str) -> ExternalSourceId {
        ExternalSourceId(self.push_node(
            name,
            NodeKind::ExternalSource { value: 0.0 },
            ClockDomain::BoardCycle,
        ))
    }

    /// A fixed value.
    pub fn constant(&mut self, name: &str, value: f64) -> NodeId {
        self.push_node(name, NodeKind::Constant { value }, ClockDomain::BoardCycle)
    }

    /// Rising-edge detector over `src`.
    pub fn edge_detector(&mut self, name: &str, src: impl Into<NodeId>) -> NodeId {
        self.push_node(
            name,
            NodeKind::EdgeDetector {
                src: src.into(),
                last: 0.0,
            },
            ClockDomain::BoardCycle,
        )
    }

    /// Square wave at a fixed frequency (Hz).
    pub fn fixed_square(&mut self, name: &str, freq: f64) -> NodeId {
        self.push_node(
            name,
            NodeKind::FixedSquare { freq, phase: 0.0 },
            ClockDomain::BoardCycle,
        )
    }

    /// Square wave whose frequency (Hz) is read from `freq_src`.
    pub fn variable_square(&mut self, name: &str, freq_src: impl Into<NodeId>) -> NodeId {
        self.push_node(
            name,
            NodeKind::VariableSquare {
                freq_src: freq_src.into(),
                phase: 0.0,
            },
            ClockDomain::BoardCycle,
        )
    }

    /// Triangle wave at a fixed frequency (Hz).
    pub fn triangle(&mut self, name: &str, freq: f64) -> NodeId {
        self.push_node(
            name,
            NodeKind::FixedTriangle { freq, phase: 0.0 },
            ClockDomain::BoardCycle,
        )
    }

    /// Triangle wave whose frequency (Hz) is read from `freq_src`.
    pub fn variable_triangle(&mut self, name: &str, freq_src: impl Into<NodeId>) -> NodeId {
        self.push_node(
            name,
            NodeKind::VariableTriangle {
                freq_src: freq_src.into(),
                phase: 0.0,
            },
            ClockDomain::BoardCycle,
        )
    }

    /// LFSR noise generator clocked internally at `freq` (Hz).
    pub fn lfsr_noise(&mut self, name: &str, freq: f64, spec: LfsrSpec) -> NodeId {
        assert!(
            spec.width >= 1 && spec.width <= 32,
            "LFSR width out of range"
        );
        // An all-zero register is a fixed point only when the feedback is not
        // inverted: XOR of two zero taps shifts in another zero for ever.
        // Inverting the feedback makes it shift in a one, so zero is a perfectly
        // ordinary starting state — and it is the state a real register powers
        // up in, so refusing it would force a fictitious seed.
        assert!(
            spec.seed != 0 || spec.invert_feedback,
            "LFSR seed must be non-zero unless the feedback is inverted"
        );
        self.push_node(
            name,
            NodeKind::LfsrNoise {
                lfsr: spec.seed,
                seed: spec.seed,
                tap_a: spec.taps.0,
                tap_b: spec.taps.1,
                width: spec.width,
                toward_high: spec.shift == LfsrShift::TowardHigh,
                invert_feedback: spec.invert_feedback,
                output_feedback: spec.output == LfsrOutput::Feedback,
                freq,
                clock_acc: 0.0,
            },
            ClockDomain::BoardCycle,
        )
    }

    /// `src * gain`.
    pub fn gain(&mut self, name: &str, src: impl Into<NodeId>, gain: f64) -> NodeId {
        self.push_node(
            name,
            NodeKind::Gain {
                src: src.into(),
                gain,
            },
            ClockDomain::BoardCycle,
        )
    }

    /// Sum of `srcs`.
    pub fn add(&mut self, name: &str, srcs: &[NodeId]) -> NodeId {
        self.push_node(
            name,
            NodeKind::Add {
                srcs: srcs.to_vec(),
            },
            ClockDomain::BoardCycle,
        )
    }

    /// `a * b`.
    pub fn multiply(&mut self, name: &str, a: impl Into<NodeId>, b: impl Into<NodeId>) -> NodeId {
        self.push_node(
            name,
            NodeKind::Multiply {
                a: a.into(),
                b: b.into(),
            },
            ClockDomain::BoardCycle,
        )
    }

    /// `src` clamped to `[lo, hi]`.
    pub fn clamp(&mut self, name: &str, src: impl Into<NodeId>, lo: f64, hi: f64) -> NodeId {
        self.push_node(
            name,
            NodeKind::Clamp {
                src: src.into(),
                lo,
                hi,
            },
            ClockDomain::BoardCycle,
        )
    }

    /// One-pole RC low-pass over `src` with the given resistance (ohms) and
    /// capacitance (farads); time constant `tau = R * C`.
    pub fn rc_low_pass(
        &mut self,
        name: &str,
        src: impl Into<NodeId>,
        ohms: f64,
        farads: f64,
    ) -> NodeId {
        self.push_node(
            name,
            NodeKind::RcLowPass {
                src: src.into(),
                tau: derive::rc_tau(ohms, farads),
                y: 0.0,
            },
            ClockDomain::BoardCycle,
        )
    }

    /// First-order low-pass specified by cutoff frequency (Hz).
    pub fn low_pass_hz(&mut self, name: &str, src: impl Into<NodeId>, cutoff_hz: f64) -> NodeId {
        let tau = derive::tau_from_cutoff_hz(cutoff_hz);
        self.push_node(
            name,
            NodeKind::RcLowPass {
                src: src.into(),
                tau,
                y: 0.0,
            },
            ClockDomain::BoardCycle,
        )
    }

    /// One-pole RC high-pass / coupling capacitor over `src`; `tau = R * C`.
    pub fn rc_high_pass(
        &mut self,
        name: &str,
        src: impl Into<NodeId>,
        ohms: f64,
        farads: f64,
    ) -> NodeId {
        self.push_node(
            name,
            NodeKind::RcHighPass {
                src: src.into(),
                tau: derive::rc_tau(ohms, farads),
                x_prev: 0.0,
                y: 0.0,
            },
            ClockDomain::BoardCycle,
        )
    }

    /// Asymmetric RC envelope charging toward `src` with separate rise and fall
    /// time constants (seconds) — capacitor charge/discharge behavior.
    pub fn rc_envelope(
        &mut self,
        name: &str,
        src: impl Into<NodeId>,
        tau_charge: f64,
        tau_discharge: f64,
    ) -> NodeId {
        self.push_node(
            name,
            NodeKind::RcEnvelope {
                src: src.into(),
                tau_charge,
                tau_discharge,
                v: 0.0,
            },
            ClockDomain::BoardCycle,
        )
    }

    /// Second-order state-variable filter at center/cutoff `f0` (Hz) and `q`.
    pub fn second_order(
        &mut self,
        name: &str,
        src: impl Into<NodeId>,
        mode: FilterMode,
        f0: f64,
        q: f64,
    ) -> NodeId {
        self.push_node(
            name,
            NodeKind::SecondOrder {
                src: src.into(),
                mode,
                f0,
                q,
                low: 0.0,
                band: 0.0,
            },
            ClockDomain::BoardCycle,
        )
    }

    /// Band-pass convenience for [`second_order`](Self::second_order).
    pub fn band_pass(
        &mut self,
        name: &str,
        src: impl Into<NodeId>,
        center_hz: f64,
        q: f64,
    ) -> NodeId {
        self.second_order(name, src, FilterMode::BandPass, center_hz, q)
    }

    /// Passive resistor mixer: weighted average of `(node, ohms)` taps, with an
    /// optional load resistor (ohms) to the reference. Output is
    /// `Σ(Vi / Ri) / (Σ(1 / Ri) + 1 / load)`.
    pub fn resistor_mixer(
        &mut self,
        name: &str,
        taps: &[(NodeId, f64)],
        load_ohms: Option<f64>,
    ) -> NodeId {
        let (srcs, total_g) = derive::resistor_mixer_conductances(taps, load_ohms);
        self.push_node(
            name,
            NodeKind::ResistorMixer { srcs, total_g },
            ClockDomain::BoardCycle,
        )
    }

    /// Diode-OR mixer: the highest input wins, less a forward drop (volts),
    /// and never below the reference.
    pub fn diode_mixer(&mut self, name: &str, srcs: &[NodeId], drop: f64) -> NodeId {
        let taps: Vec<(NodeId, f64)> = srcs.iter().map(|s| (*s, drop)).collect();
        self.diode_mixer_drops(name, &taps)
    }

    /// Diode-OR mixer with a per-branch forward drop (volts), for a node where
    /// the branches do not have the same number of junctions in series — two
    /// diodes on one input and one on another is a common arrangement, and the
    /// extra drop decides which branch wins near the crossover.
    pub fn diode_mixer_drops(&mut self, name: &str, taps: &[(NodeId, f64)]) -> NodeId {
        self.push_node(
            name,
            NodeKind::DiodeMixer {
                srcs: taps.to_vec(),
            },
            ClockDomain::BoardCycle,
        )
    }

    /// DAC with explicit per-bit weights (bit 0 first). Output sums the weights
    /// of the set bits of the integer code carried by `src`.
    pub fn dac_weighted(&mut self, name: &str, src: impl Into<NodeId>, weights: &[f64]) -> NodeId {
        self.push_node(
            name,
            NodeKind::DacLadder {
                src: src.into(),
                weights: weights.to_vec(),
            },
            ClockDomain::BoardCycle,
        )
    }

    /// Linear `bits`-wide R-2R DAC: full-scale code maps to `vref`.
    pub fn dac_r2r(&mut self, name: &str, src: impl Into<NodeId>, bits: u8, vref: f64) -> NodeId {
        let weights = derive::dac_r2r_weights(bits, vref);
        self.dac_weighted(name, src, &weights)
    }

    /// NE555 astable oscillator (port of MAME `dsd_555_astable`) from real
    /// component values: charge resistor `r1` (ohms), discharge resistor `r2`,
    /// timing cap `c` (farads), and supply `vcc` (volts). With `cv_src = None`
    /// it free-runs near `1.49 / ((r1 + 2·r2)·c)` Hz; with a control-voltage
    /// source it modulates around that. `out_high` is the square-wave high level
    /// (MAME's desc `v_out_high`; pass `vcc - 1.2` for the chip default).
    /// `output` selects the square wave or the capacitor voltage. The
    /// charge/discharge exponents are precomputed here from `sim_rate`; the
    /// sub-sample threshold-crossing loop is dropped, which is faithful while
    /// `sim_rate` is well above the oscillator frequency.
    #[allow(clippy::too_many_arguments)]
    pub fn ne555_astable(
        &mut self,
        name: &str,
        cv_src: Option<NodeId>,
        r1: f64,
        r2: f64,
        c: f64,
        vcc: f64,
        out_high: f64,
        output: Output555,
    ) -> NodeId {
        let dt = 1.0 / self.sim_rate as f64;
        let (exp_charge, exp_discharge) = derive::ne555_astable_exponents(r1, r2, c, dt);
        let (threshold_fixed, trigger_fixed) = derive::ne555_thresholds(vcc);
        self.push_node(
            name,
            NodeKind::Ne555Astable {
                cv_src,
                exp_charge,
                exp_discharge,
                v_charge: vcc,
                threshold_fixed,
                trigger_fixed,
                out_high,
                output,
                cap_v: 0.0,
                flip_flop: true,
            },
            ClockDomain::BoardCycle,
        )
    }

    /// NE555 constant-current VCO, simple type (port of `dsd_555_cc`). A
    /// transistor current source `i = (v_cc_source − (vin + junction)) / r`
    /// (clamped ≥ 0) ramps cap `c` from 1/3·`vcc` to 2/3·`vcc`, then it snaps
    /// back; the control voltage at `vin_src` sets the slope, so higher `vin`
    /// means a slower ramp and a lower frequency. `output` selects the cap
    /// voltage (the usual VCO tap) or the square wave.
    #[allow(clippy::too_many_arguments)]
    pub fn ne555_cc(
        &mut self,
        name: &str,
        vin_src: impl Into<NodeId>,
        r: f64,
        c: f64,
        vcc: f64,
        v_cc_source: f64,
        junction: f64,
        output: Output555,
    ) -> NodeId {
        let (threshold, trigger) = derive::ne555_thresholds(vcc);
        self.push_node(
            name,
            NodeKind::Ne555Cc {
                vin_src: vin_src.into(),
                r,
                c,
                v_cc_source,
                junction,
                threshold,
                trigger,
                out_high: derive::ne555_default_out_high(vcc),
                output,
                cap_v: 0.0,
                flip_flop: true,
            },
            ClockDomain::BoardCycle,
        )
    }

    /// Op-amp multiple-feedback band-pass (port of `dst_op_amp_filt`
    /// `IS_BAND_PASS_1M`). `src` enters through the first of the `r_in` input
    /// resistors (any further entries are reference resistors to `v_ref`); `rf`
    /// is the feedback resistor and `c1`/`c2` the feedback caps. The center
    /// frequency `1/(2π·√(rTotal·rf·c1·c2))`, damping, and gain set a biquad
    /// whose coefficients are precomputed here via a pre-warped bilinear
    /// transform at `sim_rate`. The output is clamped to the op-amp rails
    /// (`v_neg .. v_pos − 1.5 V`, MAME's `OP_AMP_VP_RAIL_OFFSET`), preserving
    /// overdrive distortion. Unlike [`band_pass`](Self::band_pass) (a Chamberlin
    /// filter parameterised by center/Q) this matches the op-amp's actual R/C
    /// response.
    #[allow(clippy::too_many_arguments)]
    pub fn op_amp_band_pass(
        &mut self,
        name: &str,
        src: impl Into<NodeId>,
        r_in: &[f64],
        rf: f64,
        c1: f64,
        c2: f64,
        v_ref: f64,
        v_neg: f64,
        v_pos: f64,
    ) -> NodeId {
        assert!(!r_in.is_empty(), "op_amp_band_pass needs an input resistor");
        let k = derive::op_amp_band_pass_coeffs(r_in, rf, c1, c2, self.sim_rate as f64);
        self.push_node(
            name,
            NodeKind::OpAmpBandPass {
                src: src.into(),
                a1: k.a1,
                a2: k.a2,
                b0: k.b0,
                b2: k.b2,
                in_gain: k.in_gain,
                v_ref,
                clip_lo: v_neg,
                clip_hi: v_pos - 1.5,
                // Steady state for zero input (v = −v_ref), so no power-on ring.
                x1: -v_ref,
                x2: -v_ref,
                y1: 0.0,
                y2: 0.0,
            },
            ClockDomain::BoardCycle,
        )
    }

    /// Gated diode + R//C discharge (port of `dst_rcdisc5`). A 0.7 V diode feeds
    /// the parallel `r` (ohms) / `c` (farads): while `enable_src` is high the cap
    /// follows the input upward instantly and decays downward with `τ = r·c`;
    /// while low it holds charge and outputs 0. Used to gate noise bursts.
    pub fn rc_disc5(
        &mut self,
        name: &str,
        in_src: impl Into<NodeId>,
        enable_src: impl Into<NodeId>,
        r: f64,
        c: f64,
    ) -> NodeId {
        let dt = 1.0 / self.sim_rate as f64;
        self.push_node(
            name,
            NodeKind::RcDisc5 {
                in_src: in_src.into(),
                enable_src: enable_src.into(),
                charge_exp: derive::rc_charge_exp(r, c, dt),
                cap_v: 0.0,
            },
            ClockDomain::BoardCycle,
        )
    }

    /// Comparator: `1.0` while `src` is above `level`, else `0.0`.
    ///
    /// The board shape this models is a decaying spike watched by a comparator,
    /// which is how a latch write becomes a trigger of the circuit's own
    /// choosing rather than the game's. The resulting width is not a constant —
    /// it depends on how far the spike started above the reference, which in
    /// turn depends on what the driving network was doing beforehand.
    pub fn threshold(&mut self, name: &str, src: impl Into<NodeId>, level: f64) -> NodeId {
        self.push_node(
            name,
            NodeKind::Threshold {
                src: src.into(),
                level,
            },
            ClockDomain::BoardCycle,
        )
    }

    /// Logic-triggered RC discharge, gated and modulated by a second input
    /// (port of `dst_rcdisc_mod`). `trigger_src` pulls the network to ground
    /// while asserted and releases it to charge toward `v_supply`; the output is
    /// the voltage still across the charging resistor, so it is a decaying
    /// envelope. `modulator_src` switches `r3` in and out — changing the decay
    /// rate — and chops the output to zero whenever it is above 0.6 V.
    ///
    /// The chopping is the part worth understanding. Feeding an oscillator into
    /// `modulator_src` does NOT give the same result as multiplying that
    /// oscillator by an envelope: the output is a train of one-sided pulses,
    /// present only in the oscillator's low phase, and a one-sided train carries
    /// far more low-frequency energy than the symmetric product does.
    ///
    /// Tie `modulator_src` to a constant 0 and it degenerates into a fixed-width
    /// pulse from the trigger edge, whose width depends on the resistor network
    /// rather than on how long the trigger is held.
    #[allow(clippy::too_many_arguments)]
    pub fn rc_disc_modulated(
        &mut self,
        name: &str,
        trigger_src: impl Into<NodeId>,
        modulator_src: impl Into<NodeId>,
        r1: f64,
        r2: f64,
        r3: f64,
        r4: f64,
        c: f64,
        v_supply: f64,
    ) -> NodeId {
        let dt = 1.0 / self.sim_rate as f64;
        // The trigger switches r1 in or out; the modulator switches r3 across r4.
        let rc = [(r1 + r2).max(1.0), r2.max(1.0)];
        let rc2 = [r4, r3 * r4 / (r3 + r4)];
        let mut exp_high = [0.0; 4];
        let mut vd_gain = [0.0; 4];
        for m in 0..2 {
            for t in 0..2 {
                exp_high[(m << 1) | t] = derive::rc_decay_exp(rc[t] + rc2[m], c, dt);
                vd_gain[(m << 1) | t] = rc2[m] / (rc[t] + rc2[m]);
            }
        }
        self.push_node(
            name,
            NodeKind::RcDiscModulated {
                trigger_src: trigger_src.into(),
                modulator_src: modulator_src.into(),
                v_supply,
                exp_high,
                vd_gain,
                exp_low: [
                    derive::rc_decay_exp(rc[0], c, dt),
                    derive::rc_decay_exp(rc[1], c, dt),
                ],
                gain: [r4 / (rc[0] + r4), r4 / (rc[1] + r4)],
                v_cap: 0.0,
            },
            ClockDomain::BoardCycle,
        )
    }

    /// CMOS inverter relaxation oscillator from component values: a ring of
    /// inverters with timing resistor `r` (ohms) and capacitor `c` (farads), and
    /// a bias resistor `r_bias` (ohms) limiting current through the input's
    /// protection diodes. Output is the driving gate's level in volts, so it
    /// swings between the inverter's output rails.
    ///
    /// Reach for this instead of a fixed square at a frequency read off the RC
    /// corner. `1/(2πRC)` is not this circuit's rate and neither is any other
    /// simple expression: the period is a fixed multiple of `R·C`, but the
    /// multiple depends on where the gate chain actually switches. Modelled
    /// against two oscillators measured on hardware — a three-stage at 1.85 τ
    /// and a two-stage at 1.96 τ — this predicts their periods to 0.5 % and 3 %.
    /// Assuming an ideal mid-supply threshold instead gives 2.20 τ for both,
    /// missing each by ~20 % and unable to tell them apart at all; treating the
    /// datasheet thresholds as hysteresis is worse still, out by +91 % and −41 %
    /// on the same two circuits.
    pub fn inverter_osc(
        &mut self,
        name: &str,
        topology: InverterOsc,
        r: f64,
        r_bias: f64,
        c: f64,
        gate: CmosInverter,
    ) -> NodeId {
        let dt = 1.0 / self.sim_rate as f64;
        let (tf_a, tf_b) = derive::cmos_transfer_curve(&gate);
        let r_par = r * r_bias / (r + r_bias);
        self.push_node(
            name,
            NodeKind::InverterOsc {
                three_stage: topology == InverterOsc::ThreeStage,
                v_supply: gate.v_supply,
                clamp: gate.input_clamp,
                tf_a,
                tf_b,
                exp_free: derive::rc_charge_exp(r, c, dt),
                exp_clamped: derive::rc_charge_exp(r_par, c, dt),
                ratio: r_bias / (r_bias + r),
                v_cap: 0.0,
                v_mid_prev: 0.0,
            },
            ClockDomain::BoardCycle,
        )
    }

    /// Binary counter wired as a divider: counts `clock_src`'s rising edges and
    /// outputs its top bit, so one output period spans `divisor` edges with even
    /// duty. Output is 0.0 or 1.0; scale it with [`gain`](Self::gain) to reach a
    /// logic level in volts.
    ///
    /// The clock does not have to be periodic. Driven by a noise source this
    /// divides the *edges*, which gives a square whose period wanders with the
    /// source's run lengths — a rumble with a fundamental, not the spectral tilt
    /// that low-passing the same noise to the same average frequency produces.
    /// Boards reach for this to get a low frequency out of a fast source, and it
    /// is not interchangeable with a filter.
    pub fn edge_divider(
        &mut self,
        name: &str,
        clock_src: impl Into<NodeId>,
        divisor: u32,
    ) -> NodeId {
        assert!(divisor >= 2, "edge divider needs a divisor of at least 2");
        self.push_node(
            name,
            NodeKind::EdgeDivider {
                clock_src: clock_src.into(),
                divisor,
                count: 0,
                level: false,
                last: 0.0,
            },
            ClockDomain::BoardCycle,
        )
    }

    /// Emitter follower charging a capacitor (port of `dst_rcintegrate`, type 1):
    /// base at `src`, emitter through `r_e` (ohms) into `c` (farads), with
    /// `r_load` (ohms) to ground. Conducting, the cap charges toward
    /// `src − v_be` with `τ = r_e·c`; cut off, it drains toward ground with
    /// `τ = (r_e + r_load)·c`.
    ///
    /// Reach for this rather than [`rc_low_pass`](Self::rc_low_pass) wherever
    /// the board buffers a node with a transistor. A low-pass would settle on
    /// the input's *mean*; this tracks its peaks and sags between them, so it
    /// keeps a square's fundamental where an averaging filter turns it into a
    /// DC level.
    #[allow(clippy::too_many_arguments)]
    pub fn rc_integrate(
        &mut self,
        name: &str,
        src: impl Into<NodeId>,
        v_be: f64,
        r_e: f64,
        r_load: f64,
        c: f64,
    ) -> NodeId {
        let dt = 1.0 / self.sim_rate as f64;
        self.push_node(
            name,
            NodeKind::RcIntegrate {
                src: src.into(),
                v_be,
                charge_exp: derive::rc_charge_exp(r_e, c, dt),
                discharge_exp: derive::rc_charge_exp(r_e + r_load, c, dt),
                cap_v: 0.0,
            },
            ClockDomain::BoardCycle,
        )
    }

    /// A circuit-specific custom component reading `inputs`.
    pub fn custom(
        &mut self,
        name: &str,
        inputs: Vec<NodeId>,
        comp: Box<dyn CustomComponent>,
    ) -> NodeId {
        self.push_node(
            name,
            NodeKind::Custom {
                inputs,
                comp,
                scratch: Vec::new(),
            },
            ClockDomain::BoardCycle,
        )
    }

    /// Override a node's clock domain (default [`ClockDomain::BoardCycle`]).
    pub fn set_domain(&mut self, node: impl Into<NodeId>, domain: ClockDomain) {
        let id = node.into();
        self.nodes[id.index()].domain = domain;
    }

    /// Designate the circuit's output node and final gain.
    pub fn output(&mut self, node: impl Into<NodeId>, gain: OutputGain) {
        self.output = Some((node.into(), gain));
    }

    /// Freeze the topology, computing the evaluation order.
    pub fn build(self) -> DiscreteCircuit {
        let n = self.nodes.len();
        let eval_order = topo_order(&self.nodes);
        DiscreteCircuit {
            nodes: self.nodes,
            names: self.names,
            values: vec![0.0; n],
            eval_order,
            output: self.output,
            board_clock_hz: self.board_clock_hz,
            output_sample_rate: self.output_sample_rate,
            sim_rate: self.sim_rate,
            sim_phase: 0,
            input_generation: 1,
            resampler: crate::audio::AudioResampler::new(self.sim_rate, self.output_sample_rate),
        }
    }
}

/// Depth-first topological sort. Edges that close a cycle (a dependency still on
/// the recursion stack) are left as back-edges and skipped, so the result is a
/// valid order for the acyclic remainder. Post-order places dependencies before
/// dependents.
fn topo_order(nodes: &[Node]) -> Vec<usize> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        White,
        Gray,
        Black,
    }
    let mut marks = vec![Mark::White; nodes.len()];
    let mut order = Vec::with_capacity(nodes.len());
    let mut deps = Vec::new();

    // Explicit stack to avoid deep recursion: (node, next-dep-cursor).
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for start in 0..nodes.len() {
        if marks[start] != Mark::White {
            continue;
        }
        marks[start] = Mark::Gray;
        stack.push((start, 0));
        while let Some(&(node, cursor)) = stack.last() {
            deps.clear();
            nodes[node].kind.deps(&mut deps);
            if cursor < deps.len() {
                stack.last_mut().unwrap().1 += 1;
                let d = deps[cursor];
                match marks[d] {
                    Mark::White => {
                        marks[d] = Mark::Gray;
                        stack.push((d, 0));
                    }
                    // Gray: back-edge (feedback) — skip. Black: already ordered.
                    Mark::Gray | Mark::Black => {}
                }
            } else {
                marks[node] = Mark::Black;
                order.push(node);
                stack.pop();
            }
        }
    }
    order
}

// ---------------------------------------------------------------------------
// Circuit runtime
// ---------------------------------------------------------------------------

/// A built discrete sound circuit. Owns node runtime state, the value slots, the
/// evaluation schedule, and the output resampler.
pub struct DiscreteCircuit {
    nodes: Vec<Node>,
    names: Vec<String>,
    values: Vec<f64>,
    eval_order: Vec<usize>,
    output: Option<(NodeId, OutputGain)>,
    board_clock_hz: u64,
    output_sample_rate: u64,
    sim_rate: u64,
    sim_phase: u64,
    input_generation: u64,
    resampler: crate::audio::AudioResampler<i16>,
}

impl DiscreteCircuit {
    /// Advance the circuit by `board_cycles` of board-clock time, producing
    /// `sim_rate / board_clock_hz * board_cycles` simulation steps (Bresenham).
    pub fn tick(&mut self, board_cycles: u64) {
        self.sim_phase += board_cycles.saturating_mul(self.sim_rate);
        while self.sim_phase >= self.board_clock_hz {
            self.sim_phase -= self.board_clock_hz;
            self.step();
        }
    }

    /// Run one simulation step: evaluate every due node in topological order,
    /// then feed the output node's value to the resampler.
    fn step(&mut self) {
        let dt = 1.0 / self.sim_rate as f64;
        let generation = self.input_generation;
        // Borrow the schedule out so the loop can mutate `nodes`/`values`
        // freely; the order never changes, so we put it straight back.
        let order = std::mem::take(&mut self.eval_order);
        for &i in &order {
            if scheduled(
                &mut self.nodes[i],
                self.sim_rate,
                self.output_sample_rate,
                generation,
            ) {
                let v = self.nodes[i].kind.eval(&self.values, dt);
                self.values[i] = v;
            }
        }
        self.eval_order = order;

        let sample = match self.output {
            Some((id, OutputGain(g))) => {
                let v = (self.values[id.index()] * g).clamp(-1.0, 1.0);
                (v * 32767.0) as i16
            }
            None => 0,
        };
        self.resampler.tick(sample);
    }

    /// Drain produced mono `i16` samples. Returns the number written.
    pub fn fill_audio(&mut self, out: &mut [i16]) -> usize {
        self.resampler.fill_audio(out)
    }

    /// The audio output rate in Hz.
    pub fn sample_rate(&self) -> u32 {
        self.output_sample_rate as u32
    }

    /// Set a logic input on/off.
    pub fn set_logic(&mut self, id: LogicInputId, on: bool) {
        if let NodeKind::LogicInput { value, .. } = &mut self.nodes[id.0.index()].kind {
            *value = if on { 1.0 } else { 0.0 };
        }
        self.input_generation += 1;
    }

    /// Set a data input's raw value.
    pub fn set_data(&mut self, id: DataInputId, value: f64) {
        if let NodeKind::DataInput { value: v, .. } = &mut self.nodes[id.0.index()].kind {
            *v = value;
        }
        self.input_generation += 1;
    }

    /// Fire a one-shot pulse input.
    pub fn pulse(&mut self, id: PulseInputId) {
        if let NodeKind::PulseInput { pending } = &mut self.nodes[id.0.index()].kind {
            *pending = true;
        }
        self.input_generation += 1;
    }

    /// Push a sample into an external-source input.
    pub fn set_external(&mut self, id: ExternalSourceId, value: f64) {
        if let NodeKind::ExternalSource { value: v } = &mut self.nodes[id.0.index()].kind {
            *v = value;
        }
        self.input_generation += 1;
    }

    /// Current held value of a node's output slot (for tests and debug views).
    pub fn value(&self, node: impl Into<NodeId>) -> f64 {
        self.values[node.into().index()]
    }

    /// Look up a node's builder-assigned name (for debug views).
    pub fn name(&self, node: impl Into<NodeId>) -> &str {
        &self.names[node.into().index()]
    }

    /// Find a node by the name its constructor gave it.
    ///
    /// For tooling that addresses a specific stage — rendering one voice on its
    /// own, say — where the caller has a name rather than the `NodeId` the
    /// builder returned. Names are not enforced unique, so this returns the
    /// first match; a circuit that reuses a name for two stages cannot be
    /// probed unambiguously, which is a reason not to.
    pub fn node_by_name(&self, name: &str) -> Option<NodeId> {
        self.names
            .iter()
            .position(|n| n == name)
            .map(|i| NodeId(i as u16))
    }

    /// Clear all runtime state, preserving topology and static configuration.
    pub fn reset(&mut self) {
        for node in &mut self.nodes {
            node.kind.reset_runtime();
            node.phase_acc = 0.0;
            node.last_gen = 0;
        }
        for v in &mut self.values {
            *v = 0.0;
        }
        self.sim_phase = 0;
        self.input_generation = 1;
        self.resampler.reset();
    }
}

/// Decide whether a node evaluates this step, advancing its per-node scheduler
/// state. Nodes that return `false` hold their previous slot value.
fn scheduled(node: &mut Node, sim_rate: u64, output_rate: u64, generation: u64) -> bool {
    match node.domain {
        ClockDomain::BoardCycle => true,
        ClockDomain::FixedFrequency(hz) => bresenham(&mut node.phase_acc, hz, sim_rate as f64),
        ClockDomain::OutputSample => {
            bresenham(&mut node.phase_acc, output_rate as f64, sim_rate as f64)
        }
        ClockDomain::EventOnly => {
            let due = node.last_gen != generation;
            node.last_gen = generation;
            due
        }
    }
}

#[inline]
fn bresenham(acc: &mut f64, hz: f64, sim_rate: f64) -> bool {
    *acc += hz / sim_rate;
    if *acc >= 1.0 {
        while *acc >= 1.0 {
            *acc -= 1.0;
        }
        true
    } else {
        false
    }
}

/// Save format: version + scheduler accumulators + per-node runtime + value
/// slots (held / back-edge state) + resampler. Topology is reconstructed by the
/// board's circuit constructor, not serialized.
impl crate::core::save_state::Saveable for DiscreteCircuit {
    fn save_state(&self, w: &mut StateWriter) {
        w.write_version(1);
        w.write_u64_le(self.sim_phase);
        w.write_u64_le(self.input_generation);
        for node in &self.nodes {
            w.write_f64_le(node.phase_acc);
            w.write_u64_le(node.last_gen);
            node.kind.save_runtime(w);
        }
        for v in &self.values {
            w.write_f64_le(*v);
        }
        self.resampler.save_state(w);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        r.read_version(1)?;
        self.sim_phase = r.read_u64_le()?;
        self.input_generation = r.read_u64_le()?;
        for node in &mut self.nodes {
            node.phase_acc = r.read_f64_le()?;
            node.last_gen = r.read_u64_le()?;
            node.kind.load_runtime(r)?;
        }
        for v in &mut self.values {
            *v = r.read_f64_le()?;
        }
        self.resampler.load_state(r)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
