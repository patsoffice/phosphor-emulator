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
//! # The gorilla is the drawing too
//!
//! A relaxation oscillator whose rate follows an envelope plus noise, gated by
//! a second envelope; see [`Gorilla`]. The structure is the drawing, but six
//! of its numbers are not, and each is marked where it is set: U17's noise
//! stage runs at unity gain (drawn, it latches), the second monostable's
//! period and a leak across C55 are measured from a recording of a real
//! board, the NOR's drive level, the Schmitt's upper threshold and U15's gain
//! law are fitted to that recording, and U15's peak gain is set by ear. With
//! them it rests near 410 Hz, dips 1.4 times from 100 to 200 ms and recovers
//! in tens of milliseconds, as the recording does.
//!
//! # The drums against recordings of a real board
//!
//! Solved from the drawing without reference to them, the drums land within a
//! few percent of the MAME sample set's recordings: bass 73.3 Hz against 73.7,
//! conga low 265 against 258, conga high 325 against 312, rim 1079 against
//! 1015. The three higher ones run sharp and decay a little fast, which is what
//! capacitor tolerance or a slow real 3614 would do to a bridged-T.
//!
//! # Three assumptions shared across voices, each stated where it is used
//!
//! - The 3614's output range: 0 V to 10.5 V on its +12 V supply, an LM324's.
//!   The part is not identified.
//! - The SN76489A's output swing, which the drawing does not give. It is
//!   chosen so the music plays exactly as loud as it did before the drums were
//!   rebuilt; see [`PSG_FULL_SWING_V`].
//! - U15, the gorilla's VCA, `G501534`, not identified: a gain law FITTED to
//!   the recording, zero below 3.0 V on CY and linear above, peaking at a gain
//!   set by ear.

use phosphor_core::core::save_state::{SaveError, StateReader, StateWriter};
use phosphor_core::device::{
    CustomComponent, DiscreteCircuit, DiscreteCircuitBuilder, ExternalSourceId, LogicInputId,
    OutputGain, PulseInputId,
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
    /// A pin driven by something outside the network, by index into the
    /// inputs a [`LinNet`] is stepped with: a logic output, a buffered
    /// envelope. The drums have none.
    In(usize),
}

impl Pin {
    /// The voltage of a pin the circuit holds, or `None` for a node or an
    /// input.
    fn held(self) -> Option<f64> {
        match self {
            Pin::Gnd => Some(0.0),
            Pin::Plus12 => Some(V12),
            Pin::Sj => Some(V6),
            Pin::N(_) | Pin::In(_) => None,
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

// ---------------------------------------------------------------------------
// The gorilla, from the drawing
// ---------------------------------------------------------------------------

/// A linear network with ideal op-amps, stepped by the trapezoidal rule, with
/// some pins driven from outside it: the gorilla's filters and summer. The
/// drums' engine without their switches, since nothing in these stages
/// changes state, so one matrix inverted once serves every step.
struct LinNet {
    size: usize,
    nodes: usize,
    resistors: Vec<(Pin, Pin, f64)>,
    capacitors: Vec<(Pin, Pin, f64)>,
    /// Each op-amp's +, -, out.
    op_amps: Vec<(usize, usize, usize)>,
    /// The inverse for each combination of op-amp states, indexed by the
    /// states as base-3 digits, op-amp 0 lowest: 0 linear, 1 at the top
    /// rail, 2 at ground.
    inverse: Vec<Vec<f64>>,
    /// The right-hand side the rails put in, before inputs and history.
    rails: [f64; MAX_UNKNOWNS],
    cap_g: Vec<f64>,
    cap_v: Vec<f64>,
    cap_i: Vec<f64>,
    rest_v: Vec<f64>,
}

impl LinNet {
    /// `op_amps` as +, -, out node indices. `rest` is each input's value at
    /// power-on, from which the operating point is solved.
    fn new(
        nodes: usize,
        resistors: Vec<(Pin, Pin, f64)>,
        capacitors: Vec<(Pin, Pin, f64)>,
        op_amps: &[(usize, usize, usize)],
        h: f64,
        rest: &[f64],
    ) -> Self {
        let size = nodes + op_amps.len();
        assert!(
            size <= MAX_UNKNOWNS,
            "a network has at most {MAX_UNKNOWNS} unknowns"
        );
        let cap_g = capacitors.iter().map(|&(_, _, c)| 2.0 * c / h).collect();
        let mut net = LinNet {
            size,
            nodes,
            cap_v: vec![0.0; capacitors.len()],
            cap_i: vec![0.0; capacitors.len()],
            resistors,
            capacitors,
            op_amps: op_amps.to_vec(),
            inverse: Vec::new(),
            rails: [0.0; MAX_UNKNOWNS],
            cap_g,
            rest_v: Vec::new(),
        };
        let matrix = |with_caps: bool, code: usize| {
            let n = size;
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
            for &(a, b, ohms) in &net.resistors {
                stamp(a, b, 1.0 / ohms);
            }
            if with_caps {
                for (&(a, b, _), &g) in net.capacitors.iter().zip(&net.cap_g) {
                    stamp(a, b, g);
                }
            }
            for (k, &(plus, minus, out)) in op_amps.iter().enumerate() {
                let row = nodes + k;
                m[out * n + row] -= 1.0;
                if op_state(code, k) == 0 {
                    m[row * n + plus] += 1.0;
                    m[row * n + minus] -= 1.0;
                } else {
                    m[row * n + out] = 1.0;
                }
            }
            m
        };
        for &(a, b, ohms) in &net.resistors {
            let g = 1.0 / ohms;
            if let (Pin::N(p), Some(v)) = (a, b.held()) {
                net.rails[p] += g * v;
            }
            if let (Pin::N(q), Some(v)) = (b, a.held()) {
                net.rails[q] += g * v;
            }
        }
        net.inverse = (0..3usize.pow(op_amps.len() as u32))
            .map(|code| invert(matrix(true, code)).expect("the network solves"))
            .collect();
        // The operating point, capacitors open, inputs at rest, every op-amp
        // in its linear range.
        let dc = invert(matrix(false, 0)).expect("the operating point solves");
        let r = net.drive(rest, false);
        let x = mul(&dc, &r[..size]);
        net.rest_v = net
            .capacitors
            .iter()
            .map(|&(a, b, _)| pin_volt(&x, rest, a) - pin_volt(&x, rest, b))
            .collect();
        net.reset();
        net
    }

    /// The right-hand side: rails, then what the inputs drive through each
    /// part, then (with capacitors) each capacitor's history.
    fn drive(&self, inputs: &[f64], with_caps: bool) -> [f64; MAX_UNKNOWNS] {
        fn from_input(r: &mut [f64], inputs: &[f64], a: Pin, b: Pin, g: f64) {
            if let (Pin::N(p), Pin::In(k)) = (a, b) {
                r[p] += g * inputs[k];
            }
            if let (Pin::N(q), Pin::In(k)) = (b, a) {
                r[q] += g * inputs[k];
            }
        }
        let mut r = self.rails;
        for &(a, b, ohms) in &self.resistors {
            from_input(&mut r, inputs, a, b, 1.0 / ohms);
        }
        if with_caps {
            for (k, &(a, b, _)) in self.capacitors.iter().enumerate() {
                let g = self.cap_g[k];
                from_input(&mut r, inputs, a, b, g);
                let history = g * self.cap_v[k] + self.cap_i[k];
                if let Pin::N(p) = a {
                    r[p] += history;
                }
                if let Pin::N(q) = b {
                    r[q] -= history;
                }
            }
        }
        r
    }

    /// One step with the inputs at these values; returns every unknown.
    ///
    /// Each op-amp is tried in its linear range, and clamped at the rail it
    /// would pass, as the drums' op-amps are.
    fn step(&mut self, inputs: &[f64]) -> [f64; MAX_UNKNOWNS] {
        let n = self.size;
        let base = self.drive(inputs, true);
        let mut code = 0;
        let mut x = [0.0; MAX_UNKNOWNS];
        for _ in 0..=self.op_amps.len() {
            let mut r = base;
            for (k, &(_, _, _)) in self.op_amps.iter().enumerate() {
                r[self.nodes + k] = match op_state(code, k) {
                    1 => OPAMP_HI_V,
                    2 => OPAMP_LO_V,
                    _ => 0.0,
                };
            }
            let m = &self.inverse[code];
            for (row, xr) in x.iter_mut().enumerate().take(n) {
                let line = &m[row * n..row * n + n];
                *xr = line.iter().zip(&r[..n]).map(|(a, b)| a * b).sum();
            }
            let mut next = code;
            for (k, &(_, _, out)) in self.op_amps.iter().enumerate() {
                let digit = 3usize.pow(k as u32);
                if op_state(code, k) == 0 {
                    if x[out] > OPAMP_HI_V {
                        next += digit;
                    } else if x[out] < OPAMP_LO_V {
                        next += 2 * digit;
                    }
                }
            }
            if next == code {
                break;
            }
            code = next;
        }
        for k in 0..self.capacitors.len() {
            let (a, b, _) = self.capacitors[k];
            let v = pin_volt(&x, inputs, a) - pin_volt(&x, inputs, b);
            self.cap_i[k] = self.cap_g[k] * (v - self.cap_v[k]) - self.cap_i[k];
            self.cap_v[k] = v;
        }
        x
    }

    fn reset(&mut self) {
        self.cap_v.clone_from(&self.rest_v);
        self.cap_i.iter_mut().for_each(|i| *i = 0.0);
    }

    fn save_state(&self, w: &mut StateWriter) {
        for (&v, &i) in self.cap_v.iter().zip(&self.cap_i) {
            w.write_f64_le(v);
            w.write_f64_le(i);
        }
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        for k in 0..self.cap_v.len() {
            self.cap_v[k] = r.read_f64_le()?;
            self.cap_i[k] = r.read_f64_le()?;
        }
        Ok(())
    }
}

/// Op-amp `k`'s state in a combination code: its base-3 digit, 0 linear, 1 at
/// the top rail, 2 at ground.
fn op_state(code: usize, k: usize) -> usize {
    (code / 3usize.pow(k as u32)) % 3
}

/// A pin's voltage in a solution, with its inputs.
fn pin_volt(x: &[f64], inputs: &[f64], pin: Pin) -> f64 {
    match pin {
        Pin::N(p) => x[p],
        Pin::In(k) => inputs[k],
        held => held.held().expect("a pin is a node, an input or held"),
    }
}

// The gorilla's parts, by the netlist's designators.
const R72: f64 = 330e3; // the pitch envelope's slow path
const R73: f64 = 1e3; // and its fast one, behind D6
const C55: f64 = 1e-6;
const R74: f64 = 1e3; // the gate envelope's fast path, behind D7
const R75: f64 = 470e3; // and its slow one
const C54: f64 = 1e-6;
const R82: f64 = 100e3; // the control voltage into the integrator's - input
const R85: f64 = 51e3; // Q2's collector leg on the same node
const C62: f64 = 0.022e-6; // the integrating capacitor
const R86: f64 = 51e3; // the Schmitt's + input from +6 V
const R87: f64 = 100e3; // and its hysteresis from its own output
const R90: f64 = 100e3; // the scaler's input
const R91: f64 = 10e3; // and its feedback
const R94: f64 = 51e3; // the gorilla's mixing resistor onto SJ
const C61: f64 = 1e-6; // in series with it

/// The first 4538B monostable's period, `R70 C52`, taking the 4538's period as
/// `R C`. The recording's pitch dip starts 101 to 103 ms after the trigger,
/// which agrees.
const MONO_A_S: f64 = 100e3 * 1e-6;

/// The second monostable's period. MEASURED from the recording, not the
/// drawing's `R71 C53`, 150 ms: the recording's dip holds flat until about
/// 200 ms, where a window closing at 150 would already have the pitch rising.
/// C53 is a 1 uF electrolytic, and a third over its marking is within what one
/// of its age can be.
const MONO_B_S: f64 = 0.200;

/// A 4001B's output high, on +5 V.
const CMOS_HIGH_V: f64 = 5.0;

/// The NOR's output high as it charges C55. FITTED to the recording's dip,
/// 1.4 times where +5 V gives 2.2. Nothing on the drawing loads U18 enough to
/// pull it this low, and only this output is lowered: the noise gate on the
/// same package and the second monostable's output stay at [`CMOS_HIGH_V`].
const NOR_HIGH_V: f64 = 3.0;

/// A resistance across C55. MEASURED from the recording, which recovers from
/// its dip with a time constant of about 30 ms where R72 C55 alone gives 330.
/// The drawing has no such part: a leaky electrolytic, or R72 printed a decade
/// high, would each do it, and this models the first.
const C55_LEAK_OHMS: f64 = 33e3;

/// The Schmitt's upper threshold, where the ramp turns it low. FITTED to the
/// recording's rest pitch, about 405 Hz: the drawing puts it at 7.52 V, R86
/// and R87 between +6 V and a 10.5 V output, which gives 282 Hz. This is the
/// threshold an output reaching only 7 V would set, and it is applied to the
/// threshold alone. The recording is normalized and says nothing of the
/// Schmitt's output level, so the square wave the scaler takes keeps the
/// assumed 0 to 10.5 V. It stands for something not yet found rather than for
/// a part.
const SCHMITT_UPPER_V: f64 = (V6 * R87 + 7.0 * R86) / (R86 + R87);

/// Q2's collector when saturated.
const Q2_SAT_V: f64 = 0.1;

// Node numbers in the gorilla's control network: the noise low-pass on U17
// pins 10/9/8, and the summer on U17 pins 12/13/14.
const LADDER_1: usize = 0;
const LADDER_2: usize = 1;
const LP_PLUS: usize = 2;
const LP_MINUS: usize = 3;
const LP_OUT: usize = 4;
const NOISE_COUPLED: usize = 5;
const SUM_MINUS: usize = 6;
const SUM_BIAS: usize = 7;
const CONTROL_V: usize = 8;

/// U15's CY voltage below which it passes nothing. FITTED to the MAME sample
/// set's `gorilla.wav`, a recording of a real board, because U15 (`G501534`)
/// is not identified and there is no part whose law this could be read from.
/// With C54's drawn 470 ms decay it makes the level hold while the gate
/// envelope is charged and fall about 25 dB in the 180 ms after, as the
/// recording does. Refitted from 2.60 V when [`MONO_B_S`] moved to 200 ms.
const U15_THRESHOLD_V: f64 = 3.00;

/// U15's gain with the gate envelope fully charged. SET BY EAR, not read: U15
/// has no external part that sets it (its RD pin carries only C60), and the
/// MAME sample set normalizes every recording, so nothing measures the roar
/// against the drums. At unity the roar sounded quiet beside them, and 2.6
/// makes its peak equal a bass drum hit's. The scaler's divide-by-10 on U16
/// just ahead of it is the kind of cut made to feed a VCA that then has gain.
const U15_MAX_GAIN: f64 = 2.6;

/// The gorilla as the board builds it.
///
/// A falling edge on its PPI bit fires both 4538B monostables. The second,
/// through D7, R74 and R75, charges C54 into the gate envelope that drives
/// U15. The NOR of the first's Q and the second's Q-bar is high only between
/// the first ending and the second ending, and through D6, R73 and R72 it
/// charges C55 into a pitch envelope.
///
/// The pitch envelope and the HM5837's noise, squared by a 4001B and low-passed
/// third order on U17, meet at the summer on U17, whose output is the control
/// voltage. That sets the rate of a relaxation oscillator: U17 integrates it
/// onto C62, and the Schmitt on U16 flips at thresholds R86 and R87 set around
/// +6 V, switching Q2, whose collector through R85 reverses the ramp. So the
/// pitch falls when the pitch envelope rises and wanders with the noise: a
/// growl. The Schmitt's square wave is scaled on U16, low-passed second order
/// on U16, and gated by U15, then reaches SJ through C61 and R94.
///
/// The integrator's output is its - input less C62's voltage, and that input
/// follows the + input at half the control voltage. So the control voltage
/// reaches the Schmitt twice: integrated, as the ramp's slope, and directly, at
/// half size. The direct path lets the noise cross a threshold before the ramp
/// does, which raises the pitch above what the slopes alone give. The model
/// integrated the output alone until 2026-09-26.
///
/// # Where this departs from the drawing, and why
///
/// The MAME sample set's `gorilla.wav` is a recording of a real board, and
/// the drums match that set within a few percent from the drawing alone. As
/// drawn, the gorilla did not: measured cycle by cycle it rested at 282 Hz
/// against the recording's 405, dipped 2.2 times against 1.4, held its dip to
/// 150 ms against 200, and recovered with R72 C55's 330 ms against about 30.
/// Every part involved reads as transcribed at 3x and 4x. A sweep of single
/// physical changes, each scored against those measurements, found no one
/// change that fits; these do, and each is marked where it is set.
///
/// - **U17's noise stage runs at unity gain.** The drawing gives it R68 and
///   R69, a gain of 2, and with the drawn capacitors that puts its poles in the
///   right half-plane: it latches to a rail instead of filtering, the control
///   voltage slams between its limits, and the pitch envelope disappears under
///   it. The recording has the pitch envelope, a clean dip and recovery, so the
///   board does not do what that reading says. R69 is left out.
/// - **The second monostable runs 200 ms**, [`MONO_B_S`], MEASURED: the
///   recording's dip holds to 200 ms. Its first, 100 ms as drawn, agrees.
/// - **C55 has a 33k leak across it**, [`C55_LEAK_OHMS`], MEASURED from the
///   recording's 30 ms recovery.
/// - **The NOR drives C55 from 3.0 V**, [`NOR_HIGH_V`], FITTED to the dip's
///   depth. Nothing drawn explains it.
/// - **The Schmitt's upper threshold is 6.34 V**, [`SCHMITT_UPPER_V`], FITTED
///   to the rest pitch. Nothing drawn explains it either.
/// - **U15, `G501534`, is NOT IDENTIFIED**, and its gain law is FITTED: zero
///   below [`U15_THRESHOLD_V`] on CY, rising linearly with the gate envelope to
///   [`U15_MAX_GAIN`] fully charged, applied to the AC part of its input. That
///   matches the recording's level envelope to 1.5 dB RMS: full while C54 is
///   charged, then a steep fade as C54 decays through the threshold. The peak
///   gain is set by ear against the drums, since the recordings are
///   normalized.
///
/// With these the roar rests near 410 Hz, dips to about 298 from 100 to
/// 200 ms, and is back within tens of milliseconds, against the recording's
/// 405, 294 and 200 ms. The recording's rest also has bursts of short cycles
/// where its dip is steady; this does not, and those may be noise in the
/// recording rather than the circuit, since its rest is 3 dB quieter than its
/// dip.
struct Gorilla {
    control: LinNet,
    smooth: LinNet,
    h: f64,
    mono_a: f64,
    mono_b: f64,
    pitch_env: f64,
    gate_env: f64,
    lfsr: u32,
    /// C62's voltage, the integrator's - input less its output.
    c62: f64,
    schmitt_high: bool,
    coupling: f64,
    #[cfg(test)]
    trace_lp: f64,
    #[cfg(test)]
    trace_cv: f64,
    #[cfg(test)]
    trace_integrator: f64,
}

impl Gorilla {
    const SEED: u32 = 0x1_2345;

    /// C62 at power-on, with the integrator's output at +6 V and the control
    /// voltage at rest: the summer's 4 V bias (R80 against R81 from +12 V)
    /// plus R79/R77 times the same 4 V across R77 with the pitch envelope at
    /// ground, 8.4 V, halved onto the - input.
    const REST_C62_V: f64 = 8.4 / 2.0 - V6;

    fn new(h: f64) -> Self {
        use Pin::*;
        // The noise low-pass and the summer, with the squared noise and the
        // buffered pitch envelope as inputs 0 and 1.
        let control = LinNet::new(
            9,
            vec![
                (In(0), N(LADDER_1), 20e3),       // R65
                (N(LADDER_1), N(LADDER_2), 20e3), // R66
                (N(LADDER_2), N(LP_PLUS), 20e3),  // R67
                // R68, with R69 left out: the stage at unity gain, where the
                // drawn gain of 2 latches. See the struct's doc comment.
                (N(LP_OUT), N(LP_MINUS), 20e3),
                (N(NOISE_COUPLED), N(SUM_MINUS), 47e3), // R78
                (In(1), N(SUM_MINUS), 20e3),            // R77
                (N(SUM_MINUS), N(CONTROL_V), 22e3),     // R79
                (Plus12, N(SUM_BIAS), 20e3),            // R80
                (N(SUM_BIAS), Gnd, 10e3),               // R81
            ],
            vec![
                (N(LADDER_1), Gnd, 0.0015e-6),       // C48
                (N(LADDER_2), N(LP_OUT), 0.0039e-6), // C50
                (N(LP_PLUS), Gnd, 220e-12),          // C49
                (N(LP_OUT), N(NOISE_COUPLED), 1e-6), // C51
                (N(SUM_BIAS), Gnd, 10e-6),           // C56
            ],
            &[
                (LP_PLUS, LP_MINUS, LP_OUT),
                (SUM_BIAS, SUM_MINUS, CONTROL_V),
            ],
            h,
            &[CMOS_HIGH_V / 2.0, 0.0],
        );
        // The output low-pass on U16 pins 12/13/14, a follower, with the
        // scaler's output as input 0.
        let smooth = LinNet::new(
            3,
            vec![
                (In(0), N(0), 15e3), // R92
                (N(0), N(1), 15e3),  // R93
            ],
            vec![
                (N(0), N(2), 0.047e-6), // C57
                (N(1), Gnd, 0.022e-6),  // C58
            ],
            &[(1, 2, 2)],
            h,
            &[V6],
        );
        Gorilla {
            control,
            smooth,
            h,
            mono_a: 0.0,
            mono_b: 0.0,
            pitch_env: 0.0,
            gate_env: 0.0,
            lfsr: Self::SEED,
            c62: Self::REST_C62_V,
            schmitt_high: true,
            coupling: 0.0,
            #[cfg(test)]
            trace_lp: 0.0,
            #[cfg(test)]
            trace_cv: 0.0,
            #[cfg(test)]
            trace_integrator: V6,
        }
    }

    /// An envelope capacitor charged through a diode and a small resistor and
    /// discharged through a large one, both from a CMOS output at `drive_v`
    /// when high, with `leak` across the capacitor.
    #[allow(clippy::too_many_arguments)]
    fn envelope(
        v: f64,
        high: bool,
        drive_v: f64,
        fast: f64,
        slow: f64,
        leak: f64,
        c: f64,
        h: f64,
    ) -> f64 {
        let drive = if high { drive_v } else { 0.0 };
        let mut i = (drive - v) / slow - v / leak;
        if drive - DIODE_DROP_V - v > 0.0 {
            i += (drive - DIODE_DROP_V - v) / (fast + DIODE_OHMS);
        }
        v + h * i / c
    }

    /// One substep; returns the gorilla's current into SJ.
    fn substep(&mut self) -> f64 {
        let h = self.h;
        let qa = self.mono_a > 0.0;
        let qb = self.mono_b > 0.0;
        self.mono_a = (self.mono_a - h).max(0.0);
        self.mono_b = (self.mono_b - h).max(0.0);

        // The NOR of the first's Q and the second's Q-bar.
        let window = !qa && qb;
        self.pitch_env = Self::envelope(
            self.pitch_env,
            window,
            NOR_HIGH_V,
            R73,
            R72,
            C55_LEAK_OHMS,
            C55,
            h,
        );
        self.gate_env = Self::envelope(
            self.gate_env,
            qb,
            CMOS_HIGH_V,
            R74,
            R75,
            f64::INFINITY,
            C54,
            h,
        );

        // The HM5837's output, halved, clamped and squared by the 4001B.
        let feedback = self.lfsr & 1;
        self.lfsr >>= 1;
        if feedback != 0 {
            self.lfsr ^= 0x1_2000;
        }
        let noise = if self.lfsr & 1 != 0 { CMOS_HIGH_V } else { 0.0 };

        let x = self.control.step(&[noise, self.pitch_env]);
        let cv = x[CONTROL_V];
        #[cfg(test)]
        {
            self.trace_lp = x[LP_OUT];
            self.trace_cv = cv;
        }

        // The integrator's inputs sit at half the control voltage (R83 and
        // R84), so R82 brings in cv/2 over R82, and Q2, when on, takes
        // cv/2 over R85 back out: the ramp's two slopes. That current
        // charges C62, and the output is the - input less C62's voltage, so
        // whatever the control voltage does reaches the Schmitt at half size
        // with no integration: the noise on it crosses a threshold early.
        let half = cv / 2.0;
        let mut into = half / R82;
        if self.schmitt_high {
            into -= (half - Q2_SAT_V).max(0.0) / R85;
        }
        self.c62 += h * into / C62;
        let integrator = (half - self.c62).clamp(OPAMP_LO_V, OPAMP_HI_V);
        self.c62 = half - integrator;
        #[cfg(test)]
        {
            self.trace_integrator = integrator;
        }
        // The thresholds R86 and R87 set about +6 V from the output; the
        // upper one FITTED (see SCHMITT_UPPER_V).
        let threshold = if self.schmitt_high {
            SCHMITT_UPPER_V
        } else {
            (V6 * R87 + OPAMP_LO_V * R86) / (R86 + R87)
        };
        if self.schmitt_high && integrator > threshold {
            self.schmitt_high = false;
        } else if !self.schmitt_high && integrator < threshold {
            self.schmitt_high = true;
        }
        let schmitt_out = if self.schmitt_high {
            OPAMP_HI_V
        } else {
            OPAMP_LO_V
        };

        // The scaler inverts about +6 V at R91/R90, then the low-pass.
        let scaled = V6 - (R91 / R90) * (schmitt_out - V6);
        let smoothed = self.smooth.step(&[scaled])[2];

        // U15, as a VCA on the signal's AC part, then C61 and R94 into SJ.
        // U15's gain, FITTED: zero below the threshold, unity with C54 fully
        // charged through D7.
        let full = CMOS_HIGH_V - DIODE_DROP_V;
        let gain = ((self.gate_env - U15_THRESHOLD_V) / (full - U15_THRESHOLD_V)).clamp(0.0, 1.0);
        let gated = (smoothed - V6) * gain * U15_MAX_GAIN;
        let current = (gated - self.coupling) / R94;
        self.coupling += h * current / C61;
        current
    }
}

impl CustomComponent for Gorilla {
    fn reset(&mut self) {
        self.control.reset();
        self.smooth.reset();
        self.mono_a = 0.0;
        self.mono_b = 0.0;
        self.pitch_env = 0.0;
        self.gate_env = 0.0;
        self.lfsr = Self::SEED;
        self.c62 = Self::REST_C62_V;
        self.schmitt_high = true;
        self.coupling = 0.0;
    }

    /// Input: `[edge]`, 1 for the one step after the PPI bit's falling edge,
    /// however short the pulse was. Both monostables fire on it, and
    /// retrigger on another. Output: the voice's share of `SOU`, in volts
    /// about +6 V.
    fn step(&mut self, inputs: &[f64], _dt: f64) -> f64 {
        if inputs[0] >= 0.5 {
            self.mono_a = MONO_A_S;
            self.mono_b = MONO_B_S;
        }
        let mut sum = 0.0;
        for _ in 0..OVERSAMPLE {
            sum += self.substep();
        }
        -R20 * sum / OVERSAMPLE as f64
    }

    fn save_state(&self, w: &mut StateWriter) {
        self.control.save_state(w);
        self.smooth.save_state(w);
        w.write_f64_le(self.mono_a);
        w.write_f64_le(self.mono_b);
        w.write_f64_le(self.pitch_env);
        w.write_f64_le(self.gate_env);
        w.write_u32_le(self.lfsr);
        w.write_f64_le(self.c62);
        w.write_bool(self.schmitt_high);
        w.write_f64_le(self.coupling);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.control.load_state(r)?;
        self.smooth.load_state(r)?;
        self.mono_a = r.read_f64_le()?;
        self.mono_b = r.read_f64_le()?;
        self.pitch_env = r.read_f64_le()?;
        self.gate_env = r.read_f64_le()?;
        self.lfsr = r.read_u32_le()?;
        self.c62 = r.read_f64_le()?;
        self.schmitt_high = r.read_bool()?;
        self.coupling = r.read_f64_le()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Circuit
// ---------------------------------------------------------------------------

struct CongoInputs {
    psg: ExternalSourceId,
    gorilla: PulseInputId,
    bass: LogicInputId,
    conga_low: LogicInputId,
    conga_high: LogicInputId,
    rim: LogicInputId,
}

fn build_circuit() -> (DiscreteCircuit, CongoInputs) {
    let mut b = DiscreteCircuitBuilder::new(sample_rate(), sample_rate());

    let psg = b.external_source("PSG");
    let gorilla_g = b.pulse_input("GORILLA");
    let bass_g = b.logic_input("BASS");
    let conga_low_g = b.logic_input("CONGA_LOW");
    let conga_high_g = b.logic_input("CONGA_HIGH");
    let rim_g = b.logic_input("RIM");

    let h = 1.0 / (sample_rate() as f64 * OVERSAMPLE as f64);

    // The gorilla, its share of SOU in volts about +6 V.
    let gorilla = b.custom("GORILLA", vec![gorilla_g.into()], Box::new(Gorilla::new(h)));

    // The four drums, each its share of SOU in volts about +6 V.
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
    /// Port B as last written, to find the gorilla's falling edge.
    #[save(id = 2)]
    last_port_b: u8,
}

impl Default for CongoSound {
    fn default() -> Self {
        Self::new()
    }
}

impl CongoSound {
    pub fn new() -> Self {
        let (circuit, ids) = build_circuit();
        Self {
            circuit,
            ids,
            last_port_b: 0xff,
        }
    }

    /// Feed one box-filtered PSG sample (the SN76489A mix) and advance the
    /// circuit one step, producing one output sample.
    pub fn feed_psg(&mut self, sample: i16) {
        self.circuit
            .set_external(self.ids.psg, sample as f64 / 32767.0);
        self.circuit.tick(1);
    }

    /// Update the percussion from the PPI port B/C output latches.
    ///
    /// Each drum's gate is high while its bit is low, and a drum strikes when
    /// the bit returns high and its 7416 pulls down. The game holds a drum bit
    /// low for 15.6 ms, so a level sampled each step sees it.
    ///
    /// The gorilla is caught on the write instead. The game pulses PB1 low
    /// for a few microseconds, between two output samples, and the 4538B
    /// monostables trigger on that edge however short it is. So the falling
    /// edge fires a pulse the next step consumes, rather than a level that
    /// would be back high before anything read it.
    pub fn set_triggers(&mut self, port_b: u8, port_c: u8) {
        if self.last_port_b & 0x02 != 0 && port_b & 0x02 == 0 {
            self.circuit.pulse(self.ids.gorilla);
        }
        self.last_port_b = port_b;
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
        self.last_port_b = 0xff;
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

    /// The oscillator's rate in `seconds` of substeps, from its Schmitt's
    /// flips, and how much of that time U17's noise stage spent at a rail.
    fn run_gorilla(g: &mut Gorilla, seconds: f64) -> (f64, f64) {
        let n = (seconds / g.h) as usize;
        let (mut at_rail, mut flips) = (0, 0);
        let mut last = g.schmitt_high;
        for _ in 0..n {
            let _ = g.substep();
            if g.trace_lp <= OPAMP_LO_V + 1e-9 || g.trace_lp >= OPAMP_HI_V - 1e-9 {
                at_rail += 1;
            }
            if g.schmitt_high != last {
                flips += 1;
                last = g.schmitt_high;
            }
            assert!(g.trace_cv.is_finite() && g.trace_integrator.is_finite());
        }
        (flips as f64 / 2.0 / seconds, at_rail as f64 / n as f64)
    }

    /// U17's noise stage filters, rather than latching as its drawn gain of 2
    /// would, and the growl has the recording's shape (`gorilla.wav` in the
    /// MAME sample set): it rests high, dips about 1.4 times while the NOR
    /// window is open, and is back within tens of milliseconds of the window
    /// closing, where R72 C55 alone would take a third of a second.
    ///
    /// The figures here are flips counted over a span, a mean that short
    /// cycles lift above the recording's per-cycle levels (about 405 Hz at
    /// rest and 294 in the dip).
    #[test]
    fn the_growl_rests_dips_and_recovers_as_recorded() {
        let h = 1.0 / (sample_rate() as f64 * OVERSAMPLE as f64);
        let mut g = Gorilla::new(h);
        let (rest, at_rail) = run_gorilla(&mut g, 0.5);
        assert!(
            at_rail < 0.05,
            "the noise stage is at a rail {:.1} % of the time",
            at_rail * 100.0
        );
        assert!(
            (400.0..490.0).contains(&rest),
            "the growl rests at {rest:.0} Hz"
        );

        // Fire both monostables, wait out the first, and read the window.
        g.mono_a = MONO_A_S;
        g.mono_b = MONO_B_S;
        let _ = run_gorilla(&mut g, MONO_A_S + 0.005);
        let (dip, _) = run_gorilla(&mut g, MONO_B_S - MONO_A_S - 0.01);
        let ratio = rest / dip;
        assert!(
            (1.25..1.6).contains(&ratio),
            "the growl dips to {dip:.0} Hz from {rest:.0}, {ratio:.2} times"
        );

        // 60 ms after the window closes, the pitch is back within 15 %; R72
        // C55 alone would still hold it near the dip.
        let _ = run_gorilla(&mut g, 0.005 + 0.060);
        let (back, _) = run_gorilla(&mut g, 0.030);
        assert!(
            back > rest * 0.85,
            "the growl is at {back:.0} Hz 60 ms after the dip, against {rest:.0}"
        );
    }

    /// What the game actually writes: PB1 low and straight back high, a few
    /// microseconds apart (0x7D then 0x7F, five times 0.41 s apart, from a
    /// MAME trace of a game), with no output sample between the two writes.
    /// A level sampled per step never sees that; the gorilla was silent in
    /// play until the edge was caught on the write.
    #[test]
    fn a_microsecond_pulse_on_pb1_still_fires_the_gorilla() {
        let mut snd = CongoSound::new();
        let _ = render(&mut snd, 100, 0x7f, 0x7f);
        snd.set_triggers(0x7d, 0x7f);
        snd.set_triggers(0x7f, 0x7f);
        let out = render(&mut snd, 60, 0x7f, 0x7f);
        assert!(rms(&out) > 300.0, "rms {} after the pulse", rms(&out));
    }

    /// Both monostables fire on the bit's falling edge, so the gorilla
    /// sounds from the press, and fades with the gate envelope (C54 through
    /// R75, 470 ms) back to silence.
    #[test]
    fn the_gorilla_sounds_on_its_bit_and_fades_with_its_gate_envelope() {
        let mut snd = CongoSound::new();
        let idle = render(&mut snd, 100, 0xff, 0xff);
        assert_eq!(rms(&idle), 0.0, "silent until triggered");
        let early = render(&mut snd, 60, 0xfd, 0xff);
        let _ = render(&mut snd, 1000, 0xff, 0xff);
        let late = render(&mut snd, 200, 0xff, 0xff);
        assert!(
            rms(&early) > 300.0,
            "rms {} just after the press",
            rms(&early)
        );
        assert!(
            rms(&late) < rms(&early) / 4.0,
            "rms {} a second later, against {} at the start",
            rms(&late),
            rms(&early)
        );
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
