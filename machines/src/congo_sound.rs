//! Congo Bongo's sound board, 834-5168: two SN76489A PSGs and five analog
//! percussion voices, each voice triggered by a bit of the sound board's
//! i8255 PPI. Transcribed at pin level in
//! `docs/schematics/netlists/congo-sound.toml`; the argument is in
//! `docs/schematics/congo-percussion.md`.
//!
//! # The four drums are the drawing
//!
//! Bass, low conga, high conga and rim are one circuit with four sets of
//! values, and each is built here from those values (see [`Drum`]): a 7416
//! open-collector inverter with a 10k pull-up to +12 V, a shaper (a coupling
//! capacitor, a diode that passes only the falling edge, a second capacitor),
//! and a 3614 op-amp section with a bridged-T in its feedback (a twin-T on the
//! rim) that rings at one frequency. The netlist's solver gives what they ring
//! at, and `tools/netlist/tests/congo_ac_test.rs` holds it: 73.4, 265.5, 325.2
//! and 1079.9 Hz, with Q of 10.8, 27.4, 33.6 and 23.3.
//!
//! **A drum strikes when its PPI bit goes back high**, not when it goes low.
//! The 7416 inverts the bit, so a low bit releases its output to the pull-up,
//! a rising edge the diode blocks, and it is the bit's return that pulls the
//! output down and sends the edge through. The strike therefore comes at the
//! end of whatever pulse the game writes.
//!
//! Each drum's output reaches SJ through its own coupling capacitor and mixing
//! resistor, and SJ is the second output summer's virtual ground, so the balance
//! between the voices is resistor ratios against `R20` and nothing else.
//!
//! # What is still by ear
//!
//! The gorilla. The board makes it with a relaxation oscillator whose rate
//! follows an envelope plus low-passed noise, gated by an unidentified VCA
//! (U15, `G501534`); this still synthesizes it as enveloped noise with a
//! tremolo, at the level it always had.
//!
//! # Two assumptions, both stated where they are used
//!
//! - The 3614's output range: 0 V to 10.5 V on its +12 V supply, an LM324's.
//!   The part is not identified.
//! - The SN76489A's output swing, which the drawing does not give. It is
//!   chosen so the music plays exactly as loud as it did before the drums were
//!   rebuilt; see [`PSG_FULL_SWING_V`].

use std::f64::consts::TAU;

use phosphor_core::core::save_state::{SaveError, StateReader, StateWriter};
use phosphor_core::device::{
    CustomComponent, DiscreteCircuit, DiscreteCircuitBuilder, ExternalSourceId, FilterMode,
    LogicInputId, OutputGain,
};
use phosphor_macros::Saveable;

fn sample_rate() -> u64 {
    phosphor_core::audio::host_sample_rate() as u64
}

// ---------------------------------------------------------------------------
// The output stage, sheet 1: two summers on U12 around +6 V
// ---------------------------------------------------------------------------

/// The +6 V rail every summer's + input sits on, from U14.
const V6: f64 = 6.0;
/// The +12 V rail the 3614s and the 7416 pull-ups run from.
const V12: f64 = 12.0;

/// The 3614's output range on its +12 V supply. NOT READ: the part is not
/// identified, and this is an LM324's, which reaches ground and stops 1.5 V
/// short of its positive rail.
const OPAMP_LO_V: f64 = 0.0;
const OPAMP_HI_V: f64 = V12 - 1.5;

/// The second summer's feedback. Every voice reaches `SOU` at `R20` over its
/// own mixing resistor, because SJ is this summer's virtual ground.
const R20: f64 = 20_000.0;

/// `SOU`'s largest symmetric swing about +6 V: up to [`OPAMP_HI_V`] is 4.5 V,
/// down to ground is 6. This is what renders as a full-scale sample.
const FULL_SCALE_V: f64 = OPAMP_HI_V - V6;

/// Each PSG's path to `SOU`: the first summer at `R18/R17`, 100k/51k, then the
/// second at `R20/R19`, 20k/51k.
const PSG_PATH_GAIN: f64 = (100.0 / 51.0) * (20.0 / 51.0);

/// The PSG coupling, `C14`/`C15` 1 uF into `R16`/`R17` 51k at a virtual ground.
const PSG_COUPLING_TAU: f64 = 51_000.0 * 1e-6;

/// What a PSG sample of full scale is in volts at the chip's output.
///
/// NOT FROM THE DRAWING, which gives no SN76489A output level. It is chosen so
/// that the PSGs reach the output exactly as loud as they did before the drums
/// were rebuilt from the schematic, when a full-scale PSG sample played at 0.45
/// of full scale: 0.45 of [`FULL_SCALE_V`] through [`PSG_PATH_GAIN`]. The drums'
/// level is the drawing's; the music's is this.
const PSG_FULL_SWING_V: f64 = 0.45 * FULL_SCALE_V / PSG_PATH_GAIN;

// The gorilla, still synthesized by ear at the level it always had.
const GORILLA_ATTACK_MS: f64 = 90.0;
const GORILLA_DECAY_MS: f64 = 230.0;
const GORILLA_TREMOLO_HZ: f64 = 28.0;
const GORILLA_LP_HZ: f64 = 700.0;
const GORILLA_GAIN: f64 = 0.85;

/// Convert a "decay to 10%" time in ms to an exponential time constant (s).
fn decay_tau(ms: f64) -> f64 {
    ms / 1000.0 / std::f64::consts::LN_10
}

const ENV_FLOOR: f64 = 1e-4;

// ---------------------------------------------------------------------------
// The drums, from the drawing
// ---------------------------------------------------------------------------

/// Substeps per output sample. The rim's strike decays in 57 us, under three
/// samples at 48 kHz; two substeps put it at five or six. The trapezoidal
/// rule does not damp the resonances at any step, and warps the rim's
/// 1080 Hz by well under a tenth of a percent at this one, which
/// `each_drum_rings_at_its_solved_frequency` holds to a percent. Four
/// substeps cost a third more emulation time for no difference it can see.
const OVERSAMPLE: usize = 2;

/// Where a part's end is: ground, a rail, or one of the voice's own nodes.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Pin {
    Gnd,
    Plus12,
    /// SJ, the second summer's virtual ground, held at +6 V.
    Sj,
    N(usize),
}

impl Pin {
    /// The voltage of a pin the circuit holds, or `None` for a node.
    fn held(self) -> Option<f64> {
        match self {
            Pin::Gnd => Some(0.0),
            Pin::Plus12 => Some(V12),
            Pin::Sj => Some(V6),
            Pin::N(_) => None,
        }
    }
}

/// The largest drum network: the rim's eleven nodes and its op-amp's current.
const MAX_UNKNOWNS: usize = 12;

/// The 7416's output when it sinks, a saturated open collector.
const SINK_OHMS: f64 = 30.0;
/// A conducting 1S2075: its drop and its resistance, silicon defaults, the
/// same as the netlist solver's.
const DIODE_DROP_V: f64 = 0.6;
const DIODE_OHMS: f64 = 10.0;

/// One drum's parts, by node. Every value is the netlist's; the nodes are
/// named in [`Drum::from_parts`]' callers after the netlist's nets.
struct DrumParts {
    /// How many nodes the voice has.
    nodes: usize,
    /// The 7416's output: pulled up by a resistor in `resistors`, and sunk to
    /// ground by the switch while the PPI bit is high.
    pulse: usize,
    /// The shaper's diode, anode then cathode.
    diode: (usize, usize),
    /// The op-amp section: +, -, out.
    op_amp: (usize, usize, usize),
    /// The node between the output capacitor and the mixing resistor, and
    /// the mixing resistor's ohms: the voice's current into SJ.
    mix: (usize, f64),
    resistors: Vec<(Pin, Pin, f64)>,
    capacitors: Vec<(Pin, Pin, f64)>,
}

/// A drum voice as the board builds it: a small piecewise-linear network.
///
/// Linear between switch events, so each combination of the three things that
/// switch (the 7416 sinking or not, the diode conducting or not, the op-amp in
/// its linear range or clamped at either rail) is one fixed matrix, inverted
/// once when the voice is built. A step is then one matrix-vector product, with
/// the diode and the op-amp each deciding their state from the solution and
/// the step repeated if either changed its mind. That decision is the nonlinear
/// part the netlist solver deliberately leaves to the device.
///
/// Capacitors are integrated by the trapezoidal rule, which does not damp a
/// resonance the way backward Euler would; at these Qs that would shorten
/// every ring.
///
/// This is a hand-built voice with a fixed topology, not a netlist parser: the
/// runtime reads no transcription.
struct Drum {
    parts: DrumParts,
    /// Unknowns: every node, then the op-amp's output current.
    size: usize,
    /// Each capacitor's companion conductance, `2C/h`.
    cap_g: Vec<f64>,
    /// The inverse system matrix for each state: index
    /// `sink * 6 + diode * 3 + op`, op 0 linear, 1 at the top rail, 2 at
    /// ground.
    inverse: Vec<Vec<f64>>,
    /// Each state's right-hand side without the capacitors' history.
    rhs_const: Vec<[f64; MAX_UNKNOWNS]>,
    /// Each capacitor's voltage (a minus b) and current (a to b) at the last
    /// step.
    cap_v: Vec<f64>,
    cap_i: Vec<f64>,
    diode_on: bool,
    /// The operating point at rest, which `reset` returns the capacitors to.
    rest_v: Vec<f64>,
}

impl Drum {
    fn from_parts(parts: DrumParts, h: f64) -> Self {
        let size = parts.nodes + 1;
        let cap_g: Vec<f64> = parts
            .capacitors
            .iter()
            .map(|&(_, _, c)| 2.0 * c / h)
            .collect();
        let mut drum = Drum {
            size,
            cap_g,
            inverse: Vec::new(),
            rhs_const: Vec::new(),
            cap_v: vec![0.0; parts.capacitors.len()],
            cap_i: vec![0.0; parts.capacitors.len()],
            diode_on: false,
            rest_v: Vec::new(),
            parts,
        };
        assert!(
            size <= MAX_UNKNOWNS,
            "a drum has at most {MAX_UNKNOWNS} unknowns"
        );
        drum.inverse = (0..12)
            .map(|state| {
                invert(drum.matrix(state / 6 == 1, (state / 3) % 2 == 1, state % 3, true))
                    .expect("a drum's network is solvable in every state")
            })
            .collect();
        // What each state's right-hand side is before the capacitors' history
        // is added: the rails, a conducting diode's drop, a clamped rail. It
        // does not change between steps, so it is built once.
        drum.rhs_const = (0..12)
            .map(|state| {
                let mut fixed = [0.0; MAX_UNKNOWNS];
                let r = drum.rhs((state / 3) % 2 == 1, state % 3, false);
                fixed[..r.len()].copy_from_slice(&r);
                fixed
            })
            .collect();
        // At rest the PPI bit is high, so the 7416 sinks, the diode is off and
        // the op-amp sits in its linear range. Solve that with the capacitors
        // open, which is the operating point, and start every capacitor there.
        let dc = invert(drum.matrix(true, false, 0, false)).expect("the operating point solves");
        let x = mul(&dc, &drum.rhs(false, 0, false));
        drum.rest_v = drum
            .parts
            .capacitors
            .iter()
            .map(|&(a, b, _)| volt(&x, a) - volt(&x, b))
            .collect();
        drum.reset();
        drum
    }

    /// The system matrix. `with_caps` false opens every capacitor, for the
    /// operating point.
    fn matrix(&self, sink: bool, diode_on: bool, op: usize, with_caps: bool) -> Vec<f64> {
        let n = self.size;
        let mut m = vec![0.0; n * n];
        let mut stamp = |a: Pin, b: Pin, g: f64| {
            if let Pin::N(p) = a {
                m[p * n + p] += g;
            }
            if let Pin::N(q) = b {
                m[q * n + q] += g;
            }
            if let (Pin::N(p), Pin::N(q)) = (a, b) {
                m[p * n + q] -= g;
                m[q * n + p] -= g;
            }
        };
        for &(a, b, ohms) in &self.parts.resistors {
            stamp(a, b, 1.0 / ohms);
        }
        if with_caps {
            for (&(a, b, _), &g) in self.parts.capacitors.iter().zip(&self.cap_g) {
                stamp(a, b, g);
            }
        }
        if sink {
            stamp(Pin::N(self.parts.pulse), Pin::Gnd, 1.0 / SINK_OHMS);
        }
        if diode_on {
            let (a, k) = self.parts.diode;
            stamp(Pin::N(a), Pin::N(k), 1.0 / DIODE_OHMS);
        }
        let (plus, minus, out) = self.parts.op_amp;
        let row = n - 1;
        // The op-amp supplies whatever current its output node needs...
        m[out * n + row] -= 1.0;
        // ...to hold its inputs equal, or its output at a rail.
        if op == 0 {
            m[row * n + plus] += 1.0;
            m[row * n + minus] -= 1.0;
        } else {
            m[row * n + out] = 1.0;
        }
        m
    }

    /// The right-hand side: what the rails, a conducting diode's drop, the
    /// capacitors' history and a clamped op-amp's rail put into each node.
    fn rhs(&self, diode_on: bool, op: usize, with_caps: bool) -> Vec<f64> {
        let n = self.size;
        let mut r = vec![0.0; n];
        let mut inject = |a: Pin, b: Pin, g: f64, into_a: f64| {
            // A conductance from a held pin feeds the node at the other end;
            // `into_a` is a source pushing current into `a` and out of `b`.
            if let (Pin::N(p), Some(v)) = (a, b.held()) {
                r[p] += g * v;
            }
            if let (Pin::N(q), Some(v)) = (b, a.held()) {
                r[q] += g * v;
            }
            if let Pin::N(p) = a {
                r[p] += into_a;
            }
            if let Pin::N(q) = b {
                r[q] -= into_a;
            }
        };
        for &(a, b, ohms) in &self.parts.resistors {
            inject(a, b, 1.0 / ohms, 0.0);
        }
        if with_caps {
            for (k, &(a, b, _)) in self.parts.capacitors.iter().enumerate() {
                let g = self.cap_g[k];
                inject(a, b, g, g * self.cap_v[k] + self.cap_i[k]);
            }
        }
        if diode_on {
            let (a, k) = self.parts.diode;
            let g = 1.0 / DIODE_OHMS;
            inject(Pin::N(a), Pin::N(k), g, g * DIODE_DROP_V);
        }
        r[n - 1] = match op {
            1 => OPAMP_HI_V,
            2 => OPAMP_LO_V,
            _ => 0.0,
        };
        r
    }

    /// Solve one state: its constant right-hand side plus each capacitor's
    /// history, through its precomputed inverse, into a stack array.
    fn solve(&self, state: usize) -> [f64; MAX_UNKNOWNS] {
        let n = self.size;
        let mut r = self.rhs_const[state];
        for (k, &(a, b, _)) in self.parts.capacitors.iter().enumerate() {
            let history = self.cap_g[k] * self.cap_v[k] + self.cap_i[k];
            if let Pin::N(p) = a {
                r[p] += history;
            }
            if let Pin::N(q) = b {
                r[q] -= history;
            }
        }
        let m = &self.inverse[state];
        let mut x = [0.0; MAX_UNKNOWNS];
        for (row, xr) in x.iter_mut().enumerate().take(n) {
            let line = &m[row * n..row * n + n];
            *xr = line.iter().zip(&r[..n]).map(|(a, b)| a * b).sum();
        }
        x
    }

    /// Advance one substep with the 7416 sinking or released, and return the
    /// voice's current into SJ.
    fn substep(&mut self, sink: bool) -> f64 {
        let (a, k) = self.parts.diode;
        let out = self.parts.op_amp.2;
        let mut diode_on = self.diode_on;
        let mut x = [0.0; MAX_UNKNOWNS];
        // The diode and the op-amp each decide their state from a solution.
        // Three passes settle every case these networks produce.
        for _ in 0..3 {
            let solve =
                |op: usize| self.solve(usize::from(sink) * 6 + usize::from(diode_on) * 3 + op);
            x = solve(0);
            if x[out] > OPAMP_HI_V {
                x = solve(1);
            } else if x[out] < OPAMP_LO_V {
                x = solve(2);
            }
            let across = x[a] - x[k];
            let conducts = if diode_on {
                across - DIODE_DROP_V > 0.0
            } else {
                across > DIODE_DROP_V
            };
            if conducts == diode_on {
                break;
            }
            diode_on = conducts;
        }
        self.diode_on = diode_on;
        for (i, &(pa, pb, _)) in self.parts.capacitors.iter().enumerate() {
            let v = volt(&x, pa) - volt(&x, pb);
            let g = self.cap_g[i];
            self.cap_i[i] = g * (v - self.cap_v[i]) - self.cap_i[i];
            self.cap_v[i] = v;
        }
        let (mix, ohms) = self.parts.mix;
        (x[mix] - V6) / ohms
    }
}

impl CustomComponent for Drum {
    fn reset(&mut self) {
        self.cap_v.clone_from(&self.rest_v);
        self.cap_i.iter_mut().for_each(|i| *i = 0.0);
        self.diode_on = false;
    }

    /// Input: `[released]`, 1 while the PPI bit is low and the 7416 has let
    /// go. Output: the voice's share of `SOU`, in volts about +6 V: the second
    /// summer inverts its current into SJ through `R20`.
    fn step(&mut self, inputs: &[f64], _dt: f64) -> f64 {
        let sink = inputs[0] < 0.5;
        let mut sum = 0.0;
        for _ in 0..OVERSAMPLE {
            sum += self.substep(sink);
        }
        -R20 * sum / OVERSAMPLE as f64
    }

    fn save_state(&self, w: &mut StateWriter) {
        for (&v, &i) in self.cap_v.iter().zip(&self.cap_i) {
            w.write_f64_le(v);
            w.write_f64_le(i);
        }
        w.write_bool(self.diode_on);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        for k in 0..self.cap_v.len() {
            self.cap_v[k] = r.read_f64_le()?;
            self.cap_i[k] = r.read_f64_le()?;
        }
        self.diode_on = r.read_bool()?;
        Ok(())
    }
}

/// A pin's voltage in a solution.
fn volt(x: &[f64], pin: Pin) -> f64 {
    match pin {
        Pin::N(p) => x[p],
        held => held.held().expect("a pin is a node or held"),
    }
}

/// `m x` for a square matrix stored row-major.
fn mul(m: &[f64], x: &[f64]) -> Vec<f64> {
    let n = x.len();
    (0..n)
        .map(|row| (0..n).map(|col| m[row * n + col] * x[col]).sum())
        .collect()
}

/// A square matrix's inverse by Gauss-Jordan with partial pivoting, or `None`
/// where it is singular.
fn invert(mut a: Vec<f64>) -> Option<Vec<f64>> {
    let n = (a.len() as f64).sqrt() as usize;
    let mut inv = vec![0.0; n * n];
    for i in 0..n {
        inv[i * n + i] = 1.0;
    }
    for col in 0..n {
        let pivot =
            (col..n).max_by(|&p, &q| a[p * n + col].abs().total_cmp(&a[q * n + col].abs()))?;
        if a[pivot * n + col].abs() < 1e-18 {
            return None;
        }
        for j in 0..n {
            a.swap(col * n + j, pivot * n + j);
            inv.swap(col * n + j, pivot * n + j);
        }
        let d = a[col * n + col];
        for j in 0..n {
            a[col * n + j] /= d;
            inv[col * n + j] /= d;
        }
        for row in 0..n {
            if row == col {
                continue;
            }
            let f = a[row * n + col];
            if f == 0.0 {
                continue;
            }
            for j in 0..n {
                a[row * n + j] -= f * a[col * n + j];
                inv[row * n + j] -= f * inv[col * n + j];
            }
        }
    }
    Some(inv)
}

/// The values that differ between the three bridged-T drums, from the
/// netlist. The rim adds a second T and is built on its own.
struct DrumValues {
    pull_up: f64,
    input_cap: f64,
    shaper_r1: f64,
    shaper_r2: f64,
    second_cap: f64,
    series: f64,
    plus_to_gnd: f64,
    bias_to_plus: f64,
    bias_from_12: f64,
    bias_cap: f64,
    bridge: f64,
    t_caps: f64,
    t_leg: f64,
    output_cap: f64,
    mixer: f64,
}

// Node numbers, after the netlist's nets for each voice: "<voice> pulse",
// "edge", "rectified", "coupled", "+in", "bias", "-in", "T", "out", "mix", and
// the rim's "second T".
const PULSE: usize = 0;
const EDGE: usize = 1;
const RECTIFIED: usize = 2;
const COUPLED: usize = 3;
const PLUS_IN: usize = 4;
const BIAS: usize = 5;
const MINUS_IN: usize = 6;
const T_NODE: usize = 7;
const OUT: usize = 8;
const MIX_NODE: usize = 9;
const SECOND_T: usize = 10;

fn bridged_t(v: &DrumValues) -> DrumParts {
    use Pin::*;
    DrumParts {
        nodes: 10,
        pulse: PULSE,
        diode: (RECTIFIED, EDGE),
        op_amp: (PLUS_IN, MINUS_IN, OUT),
        mix: (MIX_NODE, v.mixer),
        resistors: vec![
            (Plus12, N(PULSE), v.pull_up),
            (N(EDGE), Gnd, v.shaper_r1),
            (N(RECTIFIED), Gnd, v.shaper_r2),
            (N(COUPLED), N(PLUS_IN), v.series),
            (N(PLUS_IN), Gnd, v.plus_to_gnd),
            (N(BIAS), N(PLUS_IN), v.bias_to_plus),
            (Plus12, N(BIAS), v.bias_from_12),
            (N(MINUS_IN), N(OUT), v.bridge),
            (N(T_NODE), Gnd, v.t_leg),
            (N(MIX_NODE), Sj, v.mixer),
        ],
        capacitors: vec![
            (N(PULSE), N(EDGE), v.input_cap),
            (N(COUPLED), N(RECTIFIED), v.second_cap),
            (N(BIAS), Gnd, v.bias_cap),
            (N(MINUS_IN), N(T_NODE), v.t_caps),
            (N(T_NODE), N(OUT), v.t_caps),
            (N(OUT), N(MIX_NODE), v.output_cap),
        ],
    }
}

fn bass() -> DrumParts {
    bridged_t(&DrumValues {
        pull_up: 10e3,       // R21
        input_cap: 0.068e-6, // C20
        shaper_r1: 47e3,     // R22
        shaper_r2: 47e3,     // R23
        second_cap: 1e-6,    // C21
        series: 10e3,        // R24
        plus_to_gnd: 47e3,   // R25
        bias_to_plus: 22e3,  // R26
        bias_from_12: 10e3,  // R27
        bias_cap: 47e-6,     // C22
        bridge: 470e3,       // R28
        t_caps: 0.1e-6,      // C23, C24
        t_leg: 1e3,          // R29
        output_cap: 1e-6,    // C25
        mixer: 240e3,        // R30
    })
}

fn conga_low() -> DrumParts {
    bridged_t(&DrumValues {
        pull_up: 10e3,        // R31
        input_cap: 0.068e-6,  // C26
        shaper_r1: 47e3,      // R32
        shaper_r2: 47e3,      // R33
        second_cap: 0.033e-6, // C27
        series: 47e3,         // R34
        plus_to_gnd: 47e3,    // R35
        bias_to_plus: 22e3,   // R36
        bias_from_12: 10e3,   // R37
        bias_cap: 47e-6,      // C28
        bridge: 1e6,          // R38
        t_caps: 0.033e-6,     // C30, C31
        t_leg: 330.0,         // R39
        output_cap: 1e-6,     // C29
        mixer: 390e3,         // R40
    })
}

fn conga_high() -> DrumParts {
    bridged_t(&DrumValues {
        pull_up: 10e3,        // R41
        input_cap: 0.068e-6,  // C32
        shaper_r1: 47e3,      // R42
        shaper_r2: 47e3,      // R43
        second_cap: 0.033e-6, // C33
        series: 47e3,         // R44
        plus_to_gnd: 47e3,    // R45
        bias_to_plus: 22e3,   // R46
        bias_from_12: 10e3,   // R47
        bias_cap: 47e-6,      // C34
        bridge: 1e6,          // R48
        t_caps: 0.033e-6,     // C35, C36
        t_leg: 220.0,         // R49
        output_cap: 1e-6,     // C37
        mixer: 390e3,         // R50
    })
}

/// The rim: the same circuit with smaller values, and R60, R61 and C43 as a
/// second T from the - input to the output, which makes the feedback a twin-T.
fn rim() -> DrumParts {
    let mut parts = bridged_t(&DrumValues {
        pull_up: 10e3,         // R51
        input_cap: 0.01e-6,    // C38
        shaper_r1: 22e3,       // R52
        shaper_r2: 22e3,       // R53
        second_cap: 0.0033e-6, // C39
        series: 22e3,          // R54
        plus_to_gnd: 22e3,     // R55
        bias_to_plus: 10e3,    // R56
        bias_from_12: 4.7e3,   // R57
        bias_cap: 2.2e-6,      // C40
        bridge: 1e6,           // R58
        t_caps: 0.0068e-6,     // C41, C42
        t_leg: 470.0,          // R59
        output_cap: 1e-6,      // C44
        mixer: 200e3,          // R62
    });
    parts.nodes = 11;
    parts
        .resistors
        .push((Pin::N(MINUS_IN), Pin::N(SECOND_T), 2.2e6)); // R60
    parts.resistors.push((Pin::N(SECOND_T), Pin::N(OUT), 2.2e6)); // R61
    parts
        .capacitors
        .push((Pin::N(SECOND_T), Pin::Gnd, 0.047e-6)); // C43
    parts
}

/// A noise voice (rim, gorilla): white noise shaped by an attack/decay envelope,
/// with optional tremolo for the gorilla's growl. `one_shot` ignores retriggers
/// while still sounding (matches the gorilla's "start if not playing").
struct NoiseVoice {
    attack_s: f64,
    tau: f64,
    tremolo_hz: f64,
    one_shot: bool,
    lfsr: u32,
    env: f64,
    attacking: bool,
    trem_phase: f64,
    last_gate: f64,
}

impl NoiseVoice {
    const SEED: u32 = 0x1_2345;

    fn gorilla() -> Self {
        Self::new(
            GORILLA_ATTACK_MS,
            GORILLA_DECAY_MS,
            GORILLA_TREMOLO_HZ,
            true,
        )
    }

    fn new(attack_ms: f64, decay_ms: f64, tremolo_hz: f64, one_shot: bool) -> Self {
        Self {
            attack_s: attack_ms / 1000.0,
            tau: decay_tau(decay_ms),
            tremolo_hz,
            one_shot,
            lfsr: Self::SEED,
            env: 0.0,
            attacking: false,
            trem_phase: 0.0,
            last_gate: 0.0,
        }
    }

    fn next_noise(&mut self) -> f64 {
        // 17-bit Galois LFSR.
        let feedback = self.lfsr & 1;
        self.lfsr >>= 1;
        if feedback != 0 {
            self.lfsr ^= 0x1_2000;
        }
        if self.lfsr & 1 != 0 { 1.0 } else { -1.0 }
    }
}

impl CustomComponent for NoiseVoice {
    fn reset(&mut self) {
        self.lfsr = Self::SEED;
        self.env = 0.0;
        self.attacking = false;
        self.trem_phase = 0.0;
        self.last_gate = 0.0;
    }

    fn step(&mut self, inputs: &[f64], dt: f64) -> f64 {
        let gate = inputs[0];
        if gate >= 0.5 && self.last_gate < 0.5 && !(self.one_shot && self.env > 0.01) {
            if self.attack_s > 0.0 {
                self.env = 0.0;
                self.attacking = true;
            } else {
                self.env = 1.0;
                self.attacking = false;
            }
        }
        self.last_gate = gate;
        if self.env < ENV_FLOOR && !self.attacking {
            return 0.0;
        }

        let noise = self.next_noise();
        if self.attacking {
            self.env += dt / self.attack_s;
            if self.env >= 1.0 {
                self.env = 1.0;
                self.attacking = false;
            }
        } else {
            self.env *= (-dt / self.tau).exp();
        }

        let trem = if self.tremolo_hz > 0.0 {
            let t = 0.6 + 0.4 * self.trem_phase.sin();
            self.trem_phase += TAU * self.tremolo_hz * dt;
            if self.trem_phase >= TAU {
                self.trem_phase -= TAU;
            }
            t
        } else {
            1.0
        };
        noise * self.env * trem
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.write_u32_le(self.lfsr);
        w.write_f64_le(self.env);
        w.write_bool(self.attacking);
        w.write_f64_le(self.trem_phase);
        w.write_f64_le(self.last_gate);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.lfsr = r.read_u32_le()?;
        self.env = r.read_f64_le()?;
        self.attacking = r.read_bool()?;
        self.trem_phase = r.read_f64_le()?;
        self.last_gate = r.read_f64_le()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Circuit
// ---------------------------------------------------------------------------

struct CongoInputs {
    psg: ExternalSourceId,
    gorilla: LogicInputId,
    bass: LogicInputId,
    conga_low: LogicInputId,
    conga_high: LogicInputId,
    rim: LogicInputId,
}

fn build_circuit() -> (DiscreteCircuit, CongoInputs) {
    let mut b = DiscreteCircuitBuilder::new(sample_rate(), sample_rate());

    let psg = b.external_source("PSG");
    let gorilla_g = b.logic_input("GORILLA");
    let bass_g = b.logic_input("BASS");
    let conga_low_g = b.logic_input("CONGA_LOW");
    let conga_high_g = b.logic_input("CONGA_HIGH");
    let rim_g = b.logic_input("RIM");

    // Gorilla: swelling growl → low-pass to a rumble.
    let gorilla_raw = b.custom(
        "GORILLA",
        vec![gorilla_g.into()],
        Box::new(NoiseVoice::gorilla()),
    );
    let gorilla_lp = b.second_order(
        "GORILLA_LP",
        gorilla_raw,
        FilterMode::LowPass,
        GORILLA_LP_HZ,
        0.707,
    );
    // In volts at SOU about +6 V, like everything below: its old level, which
    // was a fraction of full scale.
    let gorilla = b.gain("GORILLA_OUT", gorilla_lp, GORILLA_GAIN * FULL_SCALE_V);

    // The four drums, each its share of SOU in volts about +6 V.
    let h = 1.0 / (sample_rate() as f64 * OVERSAMPLE as f64);
    let mut drum = |name: &str, gate: LogicInputId, parts: DrumParts| {
        b.custom(
            name,
            vec![gate.into()],
            Box::new(Drum::from_parts(parts, h)),
        )
    };
    let bass = drum("BASS", bass_g, bass());
    let conga_low = drum("CONGA_LOW", conga_low_g, conga_low());
    let conga_high = drum("CONGA_HIGH", conga_high_g, conga_high());
    let rim = drum("RIM", rim_g, rim());

    // The PSGs: coupled through C14/C15 and through both summers.
    let psg_coupled = b.high_pass_tau("PSG_COUPLED", psg, PSG_COUPLING_TAU);
    let psg_sou = b.gain("PSG_SOU", psg_coupled, PSG_FULL_SWING_V * PSG_PATH_GAIN);

    // SOU, clipped where U12 clips: at ground, 6 V below the bias, and 4.5 V
    // above it.
    let mix = b.add("MIX", &[psg_sou, gorilla, bass, conga_low, conga_high, rim]);
    let sou = b.clamp("SOU", mix, OPAMP_LO_V - V6, FULL_SCALE_V);
    b.output(sou, OutputGain::linear(1.0 / FULL_SCALE_V));

    (
        b.build(),
        CongoInputs {
            psg,
            gorilla: gorilla_g,
            bass: bass_g,
            conga_low: conga_low_g,
            conga_high: conga_high_g,
            rim: rim_g,
        },
    )
}

// ---------------------------------------------------------------------------
// Device
// ---------------------------------------------------------------------------

/// Congo Bongo's sound board: the PSG mix summed with the five voices at SJ.
#[derive(Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct CongoSound {
    #[save(id = 1)]
    circuit: DiscreteCircuit,
    /// Input handles, fixed when the circuit is built.
    #[save_skip]
    ids: CongoInputs,
}

impl Default for CongoSound {
    fn default() -> Self {
        Self::new()
    }
}

impl CongoSound {
    pub fn new() -> Self {
        let (circuit, ids) = build_circuit();
        Self { circuit, ids }
    }

    /// Feed one box-filtered PSG sample (the SN76489A mix) and advance the
    /// circuit one step, producing one output sample.
    pub fn feed_psg(&mut self, sample: i16) {
        self.circuit
            .set_external(self.ids.psg, sample as f64 / 32767.0);
        self.circuit.tick(1);
    }

    /// Update the percussion gates from the PPI port B/C output latches. Each
    /// gate is high while its bit is low. A drum strikes on the gate's falling
    /// edge, when the bit returns high and its 7416 pulls down; the gorilla,
    /// still synthesized, starts on the rising one.
    pub fn set_triggers(&mut self, port_b: u8, port_c: u8) {
        self.circuit.set_logic(self.ids.gorilla, port_b & 0x02 == 0);
        self.circuit.set_logic(self.ids.bass, port_c & 0x01 == 0);
        self.circuit
            .set_logic(self.ids.conga_low, port_c & 0x02 == 0);
        self.circuit
            .set_logic(self.ids.conga_high, port_c & 0x04 == 0);
        self.circuit.set_logic(self.ids.rim, port_c & 0x08 == 0);
    }

    /// Drain produced mono `i16` samples.
    pub fn fill_audio(&mut self, out: &mut [i16]) -> usize {
        self.circuit.fill_audio(out)
    }

    pub fn reset(&mut self) {
        self.circuit.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::save_state::{Saveable as _, StateReader, StateWriter};
    fn rms(samples: &[i16]) -> f64 {
        let sum: f64 = samples.iter().map(|&s| (s as f64).powi(2)).sum();
        (sum / samples.len().max(1) as f64).sqrt()
    }

    /// Drive the circuit for `ms` with the given trigger ports and return the
    /// rendered samples.
    fn render(snd: &mut CongoSound, ms: u64, port_b: u8, port_c: u8) -> Vec<i16> {
        snd.set_triggers(port_b, port_c);
        let n = (sample_rate() * ms / 1000) as usize;
        for _ in 0..n {
            snd.feed_psg(0); // silent PSG so we measure percussion only
        }
        let mut out = vec![0i16; n + 64];
        let got = snd.fill_audio(&mut out);
        out.truncate(got);
        out
    }

    /// How long the game holds a drum's bit low: one tick of the sound CPU's
    /// periodic interrupt, from a MAME trace of its PPI writes.
    const PULSE_MS: u64 = 16;

    /// Strike one drum the way the game does, bit low for [`PULSE_MS`] then
    /// high, and return what follows the release.
    fn strike(port_c: u8, ms: u64) -> Vec<i16> {
        let mut snd = CongoSound::new();
        let _ = render(&mut snd, 50, 0xff, 0xff);
        let _ = render(&mut snd, PULSE_MS, 0xff, port_c);
        render(&mut snd, ms, 0xff, 0xff)
    }

    /// The mean frequency from upward zero crossings, interpolated between
    /// samples, of the signal less its moving average over one expected
    /// period. The average takes out the slow tail the coupling capacitors
    /// leave, which would otherwise move the crossings of a ringing voice.
    fn ring_hz(samples: &[i16], expected_hz: f64) -> f64 {
        let w = (sample_rate() as f64 / expected_hz).round() as usize;
        let x: Vec<f64> = samples.iter().map(|&s| s as f64).collect();
        let d: Vec<f64> = (w / 2..x.len() - w / 2)
            .map(|i| x[i] - x[i - w / 2..i - w / 2 + w].iter().sum::<f64>() / w as f64)
            .collect();
        let ups: Vec<f64> = d
            .windows(2)
            .enumerate()
            .filter(|(_, w)| w[0] < 0.0 && w[1] >= 0.0)
            .map(|(i, w)| i as f64 + w[0] / (w[0] - w[1]))
            .collect();
        let periods = (ups.len() - 1) as f64;
        sample_rate() as f64 * periods / (ups[ups.len() - 1] - ups[0])
    }

    #[test]
    fn idle_is_silent() {
        let mut snd = CongoSound::new();
        let out = render(&mut snd, 20, 0xff, 0xff); // all bits high = no trigger
        assert_eq!(rms(&out), 0.0);
    }

    /// Each drum rings at the frequency the netlist's solver gives for its
    /// resonator, which `congo_ac_test.rs` pins. Measured once the strike has
    /// passed and before the ring falls into the rounding: the first ten
    /// periods after five, or for the rim, which decays in 7 ms, after two.
    #[test]
    fn each_drum_rings_at_its_solved_frequency() {
        for (name, port_c, hz) in [
            ("bass", 0xfe, 73.41),
            ("conga low", 0xfd, 265.49),
            ("conga high", 0xfb, 325.17),
            ("rim", 0xf7, 1079.94),
        ] {
            let out = strike(port_c, 300);
            let period = sample_rate() as f64 / hz;
            let skip = if name == "rim" { 2.0 } else { 5.0 };
            let from = (skip * period) as usize;
            let to = from + (11.0 * period) as usize;
            let measured = ring_hz(&out[from..to], hz);
            assert!(
                (measured - hz).abs() / hz < 0.01,
                "{name} rings at {measured:.1} Hz; its resonator solves to {hz}"
            );
        }
    }

    /// A drum strikes when its bit goes back high. The 7416 inverts the bit,
    /// so a low bit releases its output to the pull-up, and that rising edge
    /// is the one its diode blocks.
    #[test]
    fn a_drum_strikes_on_release_not_on_press() {
        for port_c in [0xfe, 0xfd, 0xfb, 0xf7] {
            let mut snd = CongoSound::new();
            let _ = render(&mut snd, 50, 0xff, 0xff);
            let held = render(&mut snd, PULSE_MS, 0xff, port_c);
            let released = render(&mut snd, 60, 0xff, 0xff);
            assert!(
                rms(&held) < 1.0,
                "port C {port_c:02x}: {:.1} rms while the bit is still low",
                rms(&held)
            );
            assert!(
                rms(&released) > 50.0,
                "port C {port_c:02x}: {:.1} rms after release",
                rms(&released)
            );
        }
    }

    /// The gorilla is still synthesized, and still starts on its bit going
    /// low.
    #[test]
    fn the_gorilla_sounds_on_its_bit() {
        let mut snd = CongoSound::new();
        snd.set_triggers(0xff, 0xff);
        snd.feed_psg(0);
        let out = render(&mut snd, 60, 0xfd, 0xff);
        assert!(rms(&out) > 50.0, "rms {}", rms(&out));
    }

    /// The game's own combined hit, bass, high conga and rim at once (port C
    /// 0x72 in the trace), does not reach the output's rails: the board has the
    /// headroom, and each drum's own op-amp clips before SOU would.
    #[test]
    fn the_games_combined_hit_does_not_clip_the_output() {
        let out = strike(0xf2, 300);
        let peak = out.iter().map(|&s| (s as i32).abs()).max().unwrap();
        assert!(peak < 32767 * 9 / 10, "peak {peak}");
    }

    #[test]
    fn psg_passes_through() {
        let mut snd = CongoSound::new();
        snd.set_triggers(0xff, 0xff);
        for _ in 0..1000 {
            snd.feed_psg(20_000);
        }
        let mut out = vec![0i16; 1100];
        let n = snd.fill_audio(&mut out);
        assert!(rms(&out[..n]) > 1000.0, "PSG mix reaches the output");
    }

    #[test]
    fn save_load_round_trip() {
        let mut snd = CongoSound::new();
        let _ = render(&mut snd, 10, 0xff, 0xfe); // trigger bass (port C bit 0)

        let mut w = StateWriter::new();
        snd.save_state(&mut w);
        let bytes = w.into_vec();

        let mut restored = CongoSound::new();
        let mut r = StateReader::new(&bytes);
        restored.load_state(&mut r).unwrap();
        // Both continue identically from the saved point.
        let a = render(&mut snd, 10, 0xff, 0xff);
        let b = render(&mut restored, 10, 0xff, 0xff);
        assert_eq!(a, b);
    }
}
