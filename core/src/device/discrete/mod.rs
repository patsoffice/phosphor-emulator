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

/// Where a constant-current 555's source injects, relative to the timing cap
/// and the discharge resistor. The two arrangements charge identically and
/// discharge toward different places, so this is a topology and not a taste.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Feed555 {
    /// Source onto the capacitor node, with the discharge resistor between that
    /// node and the discharge pin. While the pin is pulling down, the source is
    /// still feeding the resistor, so the cap relaxes toward `i·r_disch` rather
    /// than toward ground. Asteroids' thump is wired this way (Q2's collector
    /// joins C33, and R51 goes on to pin 7).
    Capacitor,
    /// Source onto the discharge pin, with the discharge resistor between that
    /// pin and the capacitor. Charging is unchanged, since the current has
    /// nowhere to go but through the resistor into the cap. Discharging is not:
    /// the pin is a saturated transistor to ground, so it swallows the source's
    /// current and the cap empties through the resistor toward **ground**.
    /// Asteroids' two fire voices are wired this way (Q4/Q5's collector joins
    /// pin 7, and R57/R61 goes on to C35/C50).
    ///
    /// The difference is not small. On the saucer fire, `i·r_disch` at the top
    /// of the sweep is 1.44 V against a 1.667 V trigger, so a model with the
    /// wrong asymptote crawls the last stretch and takes three times as long to
    /// reach it.
    DischargePin,
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

/// How a board wires a 74LS123's charge path, which decides its pulse width as
/// much as the resistor and capacitor do. See
/// [`ls123`](DiscreteCircuitBuilder::ls123).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ls123Charge {
    /// Timing resistor straight to Vcc: the datasheet's own configuration.
    Direct,
    /// Timing resistor to Vcc through a diode whose cathode drives the timing
    /// pin, which is how the Nintendo boards wire theirs. Roughly half the pulse
    /// width of [`Direct`](Self::Direct) for the same parts.
    DiodeFed,
}

/// A two-input logic gate. Only the shapes a modelled board actually contains
/// are here; add one when a board needs it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LogicOp {
    /// Exclusive OR, e.g. a 74LS86 section.
    Xor,
    /// NAND, e.g. a 74LS00 section.
    Nand,
}

/// One half of a 74LS629 dual voltage-controlled oscillator, as a board wires
/// it. Every field is a designator off the drawing; see
/// [`ls629_vco`](DiscreteCircuitBuilder::ls629_vco).
///
/// The fields are named rather than positional because four resistors and
/// capacitors in a row are easy to transpose, and a transposition here is a
/// wrong pitch rather than a compile error.
#[derive(Clone, Copy, Debug)]
pub struct Ls629 {
    /// Timing capacitor across CX1/CX2 (farads). The pins carry nothing else, so
    /// this is the one component that is unambiguously the oscillator's own.
    pub c: f64,
    /// Series resistance from whatever drives the frequency-control pin (ohms).
    /// Not a filter: it divides against the pin's own impedance, so it sets how
    /// much of the driving voltage arrives.
    pub r_freq: f64,
    /// Capacitance from the frequency-control pin to ground (farads), `0.0` for
    /// none. With `r_freq` this is what makes a pitch slew rather than step.
    pub c_freq_in: f64,
    /// Voltage at whatever drives the range pin (volts), before `r_rng`.
    pub v_rng: f64,
    /// Series resistance into the range pin (ohms), `0.0` when it is tied
    /// straight to a rail.
    pub r_rng: f64,
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

    /// How many states the register visits before repeating, from its seed.
    ///
    /// **Assert this for any register whose output is meant to be noise.** A
    /// wrong shift direction does not fail, it runs a different and usually far
    /// shorter recurrence, and a short cycle is a tone rather than noise.
    ///
    /// Asteroids' thrust ran a 42-state cycle where the board runs 32767, so its
    /// "noise" repeated every 3.5 ms. No seed helped: 42 was the longest cycle
    /// any of the 65536 starting states reached, because the polynomial was not
    /// primitive in that direction at all. It survived a long time because the
    /// stage after it is a high-Q band-pass, which rings at its own resonance
    /// whatever it is fed, so every pitch and centroid check passed while the
    /// voice was structurally wrong.
    ///
    /// A full-width register is 2^n states, so this walks the sequence with
    /// Floyd's algorithm rather than remembering where it has been.
    pub fn cycle_length(&self) -> u64 {
        let step = |s: u32| {
            lfsr_advance(
                s,
                self.taps.0,
                self.taps.1,
                self.width,
                self.shift == LfsrShift::TowardHigh,
                self.invert_feedback,
            )
            .0
        };
        let (mut slow, mut fast) = (step(self.seed), step(step(self.seed)));
        while slow != fast {
            slow = step(slow);
            fast = step(step(fast));
        }
        let mut len = 1u64;
        fast = step(slow);
        while slow != fast {
            fast = step(fast);
            len += 1;
        }
        len
    }
}

/// Advance one LFSR state, returning the next state and the feedback bit.
///
/// Shared between the running node and [`LfsrSpec::cycle_length`] on purpose: a
/// cycle length computed from a different recurrence than the one that runs
/// would be worse than not checking, since it would read as a guarantee.
pub(crate) fn lfsr_advance(
    state: u32,
    tap_a: u8,
    tap_b: u8,
    width: u8,
    toward_high: bool,
    invert_feedback: bool,
) -> (u32, u32) {
    let mask = if width >= 32 {
        u32::MAX
    } else {
        (1u32 << width) - 1
    };
    let feedback = ((state >> tap_a) ^ (state >> tap_b)) & 1;
    let shifted_in = feedback ^ u32::from(invert_feedback);
    let next = if toward_high {
        ((state << 1) | shifted_in) & mask
    } else {
        (state >> 1) | (shifted_in << (width - 1))
    };
    (next, feedback)
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
        self.lfsr_inner(name, freq, None, spec)
    }

    /// LFSR noise generator clocked at the rate carried by `freq_src`, for a
    /// register whose clock is a modelled oscillator rather than a number.
    ///
    /// Reach for this rather than measuring the oscillator once and passing the
    /// hertz: a noise source's rate is the whole of its character, and a
    /// hard-coded one silently stops tracking the parts it came from.
    pub fn lfsr_noise_clocked(
        &mut self,
        name: &str,
        freq_src: impl Into<NodeId>,
        spec: LfsrSpec,
    ) -> NodeId {
        self.lfsr_inner(name, 0.0, Some(freq_src.into()), spec)
    }

    fn lfsr_inner(
        &mut self,
        name: &str,
        freq: f64,
        freq_src: Option<NodeId>,
        spec: LfsrSpec,
    ) -> NodeId {
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
                freq_src,
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

    /// Passive resistor mixer with an analog switch in some of its legs: taps
    /// are `(node, ohms, switch)`, and a leg carrying a switch is in the network
    /// only while that node is non-zero. `load_ohms` is any permanent load to
    /// the reference.
    ///
    /// This is the CD4066 pattern, and it is not the same as feeding
    /// [`resistor_mixer`](Self::resistor_mixer) from gated sources. Gating a
    /// source to zero leaves its resistor in the divider, so the leg still
    /// attenuates every other leg; opening the switch removes the resistor, so
    /// the remaining legs get *louder*. Where the switched legs carry different
    /// signals it also re-weights them against each other, which makes the
    /// switch a timbre control rather than a volume control. A board that
    /// switches one voice's resistors in and out cannot be modelled as a gain on
    /// that voice.
    pub fn resistor_mixer_switched(
        &mut self,
        name: &str,
        taps: &[(NodeId, f64, Option<NodeId>)],
        load_ohms: Option<f64>,
    ) -> NodeId {
        let srcs: Vec<(NodeId, f64, Option<NodeId>)> = taps
            .iter()
            .map(|(n, r, sw)| {
                assert!(*r > 0.0, "mixer leg {name:?} needs a positive resistance");
                (*n, 1.0 / r, *sw)
            })
            .collect();
        self.push_node(
            name,
            NodeKind::SwitchedResistorMixer {
                srcs,
                load_g: load_ohms.map_or(0.0, |r| 1.0 / r),
            },
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

    /// NE555 constant-current VCO, simple type. A transistor current source
    /// `i = (v_cc_source − (vin + junction)) / r` (clamped ≥ 0) ramps cap `c`
    /// from 1/3·`vcc` to 2/3·`vcc`; the control voltage at `vin_src` sets the
    /// slope, so higher `vin` means a slower ramp and a lower frequency.
    /// `output` selects the cap voltage or the square wave.
    ///
    /// `r_disch` is the resistance the 555's discharge pin pulls the cap down
    /// through, and it is what gives the square output a duty cycle worth
    /// tapping. Pass **0 for an ideal discharge**, where the cap snaps to the
    /// trigger in one step: that is the physical limit of the same model, and it
    /// leaves the square a pulse one step wide. [`Feed555::DischargePin`] needs a
    /// real resistance, since with none the pin would sit straight across the
    /// cap.
    ///
    /// `feed` says which side of that resistor the current source sits on, and
    /// it decides where the cap relaxes to while the discharge pin is pulling
    /// down: toward `i·r_disch` when the source feeds the cap, toward ground
    /// when it feeds the pin. Read it off the schematic rather than assuming.
    /// Asteroids has one of each, drawn a few centimetres apart, and the
    /// difference is worth three times the discharge time at the top of the
    /// fire sweep.
    ///
    /// `reset` is the 555's reset pin. While it is low the timer is held with
    /// its capacitor discharged and its output low, so releasing it always
    /// starts the voice from the same place. **Gate here rather than at the
    /// output.** Multiplying a free-running oscillator's output by an enable
    /// switches it on at whatever phase it happens to be passing, which is a
    /// step discontinuity and is audible as a scratch at every onset. Asteroids'
    /// thump had exactly that. Pass `None` for a timer that free-runs.
    #[allow(clippy::too_many_arguments)]
    pub fn ne555_cc(
        &mut self,
        name: &str,
        vin_src: impl Into<NodeId>,
        reset: Option<NodeId>,
        r: f64,
        c: f64,
        r_disch: f64,
        vcc: f64,
        v_cc_source: f64,
        junction: f64,
        feed: Feed555,
        output: Output555,
    ) -> NodeId {
        assert!(
            r_disch >= 0.0,
            "555 discharge resistance must not be negative"
        );
        assert!(
            !(feed == Feed555::DischargePin && r_disch == 0.0),
            "a source feeding the discharge pin needs a discharge resistance: \
             with none, the pin sits straight across the timing capacitor"
        );
        let (threshold, trigger) = derive::ne555_thresholds(vcc);
        // Precomputed here rather than per step: the timestep is fixed, and this
        // sits in the audio hot path.
        let discharge_alpha = if r_disch > 0.0 {
            1.0 - (-1.0 / (self.sim_rate as f64 * r_disch * c)).exp()
        } else {
            0.0
        };
        self.push_node(
            name,
            NodeKind::Ne555Cc {
                vin_src: vin_src.into(),
                reset,
                r,
                c,
                r_disch,
                discharge_alpha,
                v_cc_source,
                junction,
                feed,
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

    /// Reserve a node whose source is wired later with
    /// [`connect`](Self::connect), so a feedback loop spanning several nodes can
    /// be named.
    ///
    /// [`next_id`](Self::next_id) covers a node that reads itself, which is the
    /// only loop a builder can express by ordering alone. A real board's loops
    /// are longer: Donkey Kong Jr.'s walking voice runs an oscillator into a
    /// counter, a counter tap through a multiplexer and an inverter, and that
    /// back into the same oscillator's frequency control, so every node in the
    /// ring needs one that does not exist yet. Reserve the ring's cut point,
    /// build forward, then connect.
    ///
    /// The cut becomes a back-edge with the usual one-step delay. `build` panics
    /// if a reserved node was never connected, since a silently dead node would
    /// leave the loop open and the voice merely wrong.
    pub fn feedback_node(&mut self, name: &str) -> NodeId {
        self.push_node(
            name,
            NodeKind::Feedback { src: None },
            ClockDomain::BoardCycle,
        )
    }

    /// Wire the source of a node reserved by
    /// [`feedback_node`](Self::feedback_node).
    pub fn connect(&mut self, node: impl Into<NodeId>, src: impl Into<NodeId>) {
        let id = node.into();
        match &mut self.nodes[id.index()].kind {
            NodeKind::Feedback { src: slot } => {
                assert!(slot.is_none(), "feedback node connected twice");
                *slot = Some(src.into());
            }
            _ => panic!("connect expects a node made by feedback_node"),
        }
    }

    /// Retriggerable monostable built from a 74LS123's timing components: a
    /// rising edge on `trigger_src` drives the output high for `K·r·c` seconds,
    /// and another edge inside that window restarts it.
    ///
    /// **`charge` is not a detail — it is nearly a factor of two on the same two
    /// components.** The datasheet's `tW = 0.45·Rext·Cext` describes its own
    /// configuration, the resistor tied straight to Vcc; a board that charges
    /// the capacitor through a diode instead gets about 0.25·R·C. See
    /// [`Ls123Charge`].
    ///
    /// This was got wrong once here, in the way that is easy to defend at the
    /// time: the datasheet's constant was taken because it is the datasheet's,
    /// and it also says the diode "is not needed for electrolytic capacitance
    /// application and should not be used on the LS122 and LS123" — and these
    /// are electrolytics. The boards fit the diode anyway. A Donkey Kong Jr.
    /// footstep came out 99 ms against the board's 57 ms, which is audible
    /// immediately and measures as a wrong attack rather than a wrong spectrum.
    pub fn ls123(
        &mut self,
        name: &str,
        trigger_src: impl Into<NodeId>,
        charge: Ls123Charge,
        r: f64,
        c: f64,
    ) -> NodeId {
        self.push_node(
            name,
            NodeKind::Ls123 {
                src: trigger_src.into(),
                width: derive::ls123_pulse_width(charge, r, c),
                remaining: 0.0,
                last: 0.0,
            },
            ClockDomain::BoardCycle,
        )
    }

    /// Ripple counter (4020 and kin) clocked by the *frequency* on `freq_src`,
    /// counting modulo `1 << stages`. The node's value is the count; read one
    /// stage with [`bit_decode`](Self::bit_decode).
    ///
    /// Taking a rate rather than a waveform is deliberate: a board reaches for a
    /// counter precisely when its clock is too fast to be interesting on its
    /// own, and 59 kHz cannot be a square at any simulation rate this framework
    /// can afford. Pair it with [`ls629_vco`](Self::ls629_vco), which produces
    /// exactly such a rate.
    pub fn ripple_counter(
        &mut self,
        name: &str,
        freq_src: impl Into<NodeId>,
        stages: u8,
    ) -> NodeId {
        assert!(
            (1..=31).contains(&stages),
            "ripple counter needs 1 to 31 stages"
        );
        self.push_node(
            name,
            NodeKind::RippleCounter {
                freq_src: freq_src.into(),
                mask: (1u32 << stages) - 1,
                count: 0,
                clock_acc: 0.0,
            },
            ClockDomain::BoardCycle,
        )
    }

    /// One stage of a counter: bit `bit` of `src`'s integer value, as 0.0 or 1.0.
    /// Stage `n` of a ripple counter is a divide-by-`2^(n+1)` square.
    pub fn bit_decode(&mut self, name: &str, src: impl Into<NodeId>, bit: u8) -> NodeId {
        self.push_node(
            name,
            NodeKind::BitDecode {
                src: src.into(),
                bit,
            },
            ClockDomain::BoardCycle,
        )
    }

    /// Two-to-one selector (74LS157 and kin): `high_src` while `sel_src` is a
    /// logic one, `low_src` otherwise.
    pub fn select(
        &mut self,
        name: &str,
        sel_src: impl Into<NodeId>,
        low_src: impl Into<NodeId>,
        high_src: impl Into<NodeId>,
    ) -> NodeId {
        self.push_node(
            name,
            NodeKind::Select {
                sel_src: sel_src.into(),
                low_src: low_src.into(),
                high_src: high_src.into(),
            },
            ClockDomain::BoardCycle,
        )
    }

    /// Render a logic level as the two voltages a real gate drives, `v_low` and
    /// `v_high`.
    ///
    /// Use this wherever a gate's output is an *analog* input to something else,
    /// which on these boards it usually is. A TTL output is not 0 V and 5 V, and
    /// where it lands on an oscillator's control pin the difference between 0 V
    /// and 0.15 V is a different pitch rather than a rounding error.
    pub fn logic_levels(
        &mut self,
        name: &str,
        src: impl Into<NodeId>,
        v_low: f64,
        v_high: f64,
    ) -> NodeId {
        self.push_node(
            name,
            NodeKind::LogicLevels {
                src: src.into(),
                v_low,
                v_high,
            },
            ClockDomain::BoardCycle,
        )
    }

    /// A two-input logic gate over 0/1 levels.
    pub fn logic_gate(
        &mut self,
        name: &str,
        gate: LogicOp,
        a: impl Into<NodeId>,
        b: impl Into<NodeId>,
    ) -> NodeId {
        self.push_node(
            name,
            NodeKind::LogicGate {
                gate,
                a: a.into(),
                b: b.into(),
            },
            ClockDomain::BoardCycle,
        )
    }

    /// One 74LS629 voltage-controlled oscillator half, from the parts a board
    /// wires around it. **The returned node's value is the oscillator's
    /// frequency in hertz, not its output.** Wrap it in a
    /// [`variable_square`](Self::variable_square) where the board uses the
    /// waveform; read it as a rate where the board only counts its edges.
    ///
    /// That split is not a convenience. The '629 is specified to 20 MHz and
    /// boards use it there: on a Donkey Kong Jr. board one half runs to 59 kHz
    /// purely to clock a ripple counter, which no simulation rate this framework
    /// can afford would represent as a square. A rate can be counted exactly at
    /// any simulation rate; a square cannot.
    ///
    /// The part is a constant-current relaxation oscillator with no external
    /// timing resistor, so `c` alone sets the scale and the two pin voltages set
    /// the rate. Both pins have about 90 kΩ of their own impedance, so a series
    /// resistor into either one is a divider and belongs in the model —
    /// `r_freq` is not a filter, and `r_rng` matters even though the range pin
    /// usually sits on a rail. Note the sense of the range input, which is the
    /// easiest thing here to get backwards: raising it *lowers* the frequency.
    ///
    /// The enable pin is not modelled. Every instance across the two boards that
    /// use this grounds it, so all of them free-run; a board that switches one
    /// wants a gate at the point the circuit actually gates, which is not here.
    ///
    /// The rate itself comes from a measured surface rather than an equation —
    /// the part is non-linear and even non-monotonic below 1 V, and its
    /// datasheet publishes no law for this member of the family. See
    /// [`derive::ls629_frequency`] for what that surface is and whose
    /// measurements stand behind it.
    pub fn ls629_vco(&mut self, name: &str, fc_src: impl Into<NodeId>, part: Ls629) -> NodeId {
        assert!(part.c > 0.0, "a 74LS629 needs a timing capacitor");
        let dt = 1.0 / self.sim_rate as f64;
        let pin_r = derive::LS629_PIN_R;
        // A capacitor on the control pin charges through the series resistor in
        // parallel with the pin's own impedance, not through the resistor alone.
        let freq_in_exp = (part.c_freq_in > 0.0).then(|| {
            let r = part.r_freq * pin_r / (part.r_freq + pin_r);
            derive::rc_charge_exp(r, part.c_freq_in, dt)
        });
        self.push_node(
            name,
            NodeKind::Ls629Vco {
                fc_src: fc_src.into(),
                v_rng: part.v_rng * pin_r / (part.r_rng + pin_r),
                v_freq_scale: pin_r / (part.r_freq + pin_r),
                freq_in_exp,
                c_scale: derive::LS629_REF_C / part.c,
                v_cap: 0.0,
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
        // A reserved node nobody connected is an open feedback loop, which does
        // not fail: the voice simply runs with a dead source and sounds wrong.
        for (i, node) in self.nodes.iter().enumerate() {
            assert!(
                !matches!(node.kind, NodeKind::Feedback { src: None }),
                "feedback node '{}' was never connected",
                self.names[i]
            );
        }
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
        self.values.fill(0.0);
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
