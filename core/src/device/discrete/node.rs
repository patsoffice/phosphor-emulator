//! Primitive components for the discrete sound circuit graph.
//!
//! A [`Node`] pairs a primitive [`NodeKind`] with the per-node scheduler state
//! used by the circuit's clock domains. Topology (which node reads which) lives
//! in the `NodeId` references inside each kind and is fixed once the circuit is
//! built; everything mutated at runtime is serialized by `save_runtime`.

use super::{ClockDomain, CustomComponent, NodeId};
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
            | NodeKind::Clamp { src, .. } => out.push(src.index()),
            NodeKind::VariableSquare { freq_src, .. } => out.push(freq_src.index()),
            NodeKind::Multiply { a, b } => {
                out.push(a.index());
                out.push(b.index());
            }
            NodeKind::Add { srcs } => out.extend(srcs.iter().map(|s| s.index())),
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
            NodeKind::FixedSquare { phase, .. } | NodeKind::VariableSquare { phase, .. } => {
                *phase = 0.0
            }
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
            NodeKind::Custom { comp, .. } => comp.reset(),
            NodeKind::Constant { .. }
            | NodeKind::Gain { .. }
            | NodeKind::Add { .. }
            | NodeKind::Multiply { .. }
            | NodeKind::Clamp { .. } => {}
        }
    }

    /// Serialize the mutable runtime state of this node (not its topology).
    pub(crate) fn save_runtime(&self, w: &mut StateWriter) {
        match self {
            NodeKind::LogicInput { value, .. }
            | NodeKind::DataInput { value, .. }
            | NodeKind::ExternalSource { value } => w.write_f64_le(*value),
            NodeKind::PulseInput { pending } => w.write_bool(*pending),
            NodeKind::FixedSquare { phase, .. } | NodeKind::VariableSquare { phase, .. } => {
                w.write_f64_le(*phase)
            }
            NodeKind::LfsrNoise {
                lfsr, clock_acc, ..
            } => {
                w.write_u32_le(*lfsr);
                w.write_f64_le(*clock_acc);
            }
            NodeKind::EdgeDetector { last, .. } => w.write_f64_le(*last),
            NodeKind::Custom { comp, .. } => comp.save_state(w),
            NodeKind::Constant { .. }
            | NodeKind::Gain { .. }
            | NodeKind::Add { .. }
            | NodeKind::Multiply { .. }
            | NodeKind::Clamp { .. } => {}
        }
    }

    /// Restore the mutable runtime state written by [`save_runtime`].
    pub(crate) fn load_runtime(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        match self {
            NodeKind::LogicInput { value, .. }
            | NodeKind::DataInput { value, .. }
            | NodeKind::ExternalSource { value } => *value = r.read_f64_le()?,
            NodeKind::PulseInput { pending } => *pending = r.read_bool()?,
            NodeKind::FixedSquare { phase, .. } | NodeKind::VariableSquare { phase, .. } => {
                *phase = r.read_f64_le()?
            }
            NodeKind::LfsrNoise {
                lfsr, clock_acc, ..
            } => {
                *lfsr = r.read_u32_le()?;
                *clock_acc = r.read_f64_le()?;
            }
            NodeKind::EdgeDetector { last, .. } => *last = r.read_f64_le()?,
            NodeKind::Custom { comp, .. } => comp.load_state(r)?,
            NodeKind::Constant { .. }
            | NodeKind::Gain { .. }
            | NodeKind::Add { .. }
            | NodeKind::Multiply { .. }
            | NodeKind::Clamp { .. } => {}
        }
        Ok(())
    }
}
