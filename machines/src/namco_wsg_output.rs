//! The Namco WSG's analog output stage, built on the [`DiscreteCircuit`]
//! runtime and shared by every board in the family.
//!
//! Transcribed in
//! [`pacman-audio-output.md`](../../docs/schematics/pacman-audio-output.md) and
//! [`namco-galaga-audio-output.md`](../../docs/schematics/namco-galaga-audio-output.md).
//! Tracked as `phosphor-emulator-ga9p` (Pac-Man) and `-enst` (the rest).
//!
//! # What these boards do that the WSG does not
//!
//! The chip's four-bit sample and four-bit volume leave one 74LS273 as eight
//! separate lines, and the board multiplies them in the analog domain: four
//! outputs drive 470, 1k, 2.2k and 4.7k into one summing node, and the other
//! four gate 4066 sections that tap that node through 10k, 22k, 47k and 100k.
//! So the product `sample * volume` that
//! [`NamcoWsg::tick`](phosphor_core::device::namco_wsg::NamcoWsg::tick) forms
//! is the one quantity these boards never have, which is why the voices arrive
//! here as codes through
//! [`tick_voices`](phosphor_core::device::namco_wsg::NamcoWsg::tick_voices).
//!
//! Three mechanisms come out of the resistor values:
//!
//! - **The volume network is a divider**, not a sum of leg gains. A leg that is
//!   switched out leaves the network, so the gain is `G / (G + bias)` and the
//!   law is compressed: on Pac-Man code 1 sits 11.1 dB below full scale where
//!   `code / 15` puts it at 23.5 dB. Every decaying note decays about twice as
//!   far in dB with the multiply done in integers.
//! - **The sample ladder distorts.** Code 8, the waveform's zero, lands at
//!   0.5607 of full scale rather than 0.5. The deviation from linear is exactly
//!   odd-symmetric about code 7.5 while the model's signed zero is code 8, and
//!   that half-code offset is what mixes even harmonics into what would
//!   otherwise be an odd mechanism. Measured over Galaga's eight PROM
//!   waveforms it runs 23 to 30 dB below each waveform's own AC content.
//! - **The shunt's corner moves with the volume code**, because the code
//!   decides which resistors are in circuit. A note gets darker as it decays
//!   as well as quieter, by about an octave across the code range.
//!
//! # One law, four boards
//!
//! [`BoardParams`] is the whole difference between them: a bias conductance, a
//! shunt capacitor and a coupling. Pac-Man, Galaga, Xevious and Dig Dug share
//! all ten resistor values and differ in what the switched conductance divides
//! against. **That is a reading, not a family resemblance.** Each board's bias
//! arm was read off its own sheet, and Xevious in particular turned out to be
//! Galaga's rather than Dig Dug's, which nobody could have predicted from the
//! board family: the epic this belongs to has four counterexamples where a
//! shared board loaded its DAC differently.
//!
//! # The multiplex, and why the voices are not summed
//!
//! The boards time-multiplex one latch, one DAC and one switched network across
//! the three voices, so only one voice's legs are ever in circuit, while the
//! bias arm is permanently connected. The node therefore settles by charge
//! balance over a frame rather than by superposition:
//!
//! ```text
//! V = sum(d_i * V_i * g_i) / (bias + sum(d_i * g_i))
//! ```
//!
//! where `d_i` is a voice's share of the frame. **That is not a sum of
//! per-voice dividers**, and the two only agree when every voice carries the
//! same conductance. Summing three independent dividers, which is what this
//! module used to do, runs up to 7.2 dB quiet on Pac-Man for a voice playing
//! alone and sets the shunt's corner about twice too high. So the twelve
//! switched legs meet at [`one node`](DiscreteCircuitBuilder::resistor_mixer_switched)
//! against a single bias arm, each leg scaled by its voice's duty, and the
//! shunt's conductance falls out of the same node.
//!
//! What this still cannot show is the per-slot stepping at the multiplex rate,
//! which is above the audio band in any case.
//!
//! Both Galaga-family boards leave the PCB differentially, Dig Dug and Xevious
//! as op-amp pairs and Galaga as a bridge amplifier. All of them are mono here.

use phosphor_core::audio::host_sample_rate;
use phosphor_core::core::save_state::{SaveError, StateReader, StateWriter};
use phosphor_core::device::discrete::{
    CustomComponent, DataInputId, DiscreteCircuit, DiscreteCircuitBuilder, NodeId, OutputGain,
};
use phosphor_macros::Saveable;

// ---------------------------------------------------------------------------
// The drawing
// ---------------------------------------------------------------------------

/// The sample ladder, MSB (latch pin 12) first. Pac-Man's R9/R8/R7/R6,
/// Galaga's R97-R100, Xevious's R59/R58/R57/R56: four boards, one set of values.
const SAMPLE_LEGS: [f64; 4] = [470.0, 1_000.0, 2_200.0, 4_700.0];
/// The 4066-switched volume legs, MSB (latch pin 2) first. Also common to all
/// four boards.
const VOLUME_LEGS: [f64; 4] = [10_000.0, 22_000.0, 47_000.0, 100_000.0];

/// Each voice's share of the 96 kHz multiplex frame, read off the sequencer
/// PROM.
///
/// Sixteen slots of four dot clocks pass in one 64-dot frame, and the 74LS273
/// holds its contents until the next clock, so a voice's weight at the node is
/// how many slots pass before the latch is reloaded. The PROM's four outputs
/// partition the frame: two of them strobe in thirteen slots, and the other two
/// strobe three times each, at slots 5, 10 and 15. Those three are the voice
/// slots, and the gaps between them are **5, 5 and 6 slots, summing to 16 of
/// 16**, which is also what confirms the latch never sits idle.
///
/// So the voices are **not** weighted evenly: one sits 1.02 dB above an even
/// third and the other two 0.56 dB below, a 1.58 dB spread. They still sum to
/// one, which is why [`full_swing`] does not depend on the split.
///
/// The same 256x4 part is in every ROM set this stage serves, byte for byte:
/// `82s126.3m` on Pac-Man and Ms. Pac-Man, `prom-2.5c` on all four Galaga
/// revisions, `xvi-1.5n` on Xevious, `136007.109` on the three Dig Dugs and
/// `bos1-2.5c` on Bosconian, SHA-1 `0c4d0bee858b97632411c440bea6948a74759746`.
/// It is a reading rather than a family resemblance, which matters on a board
/// family that has four counterexamples.
///
/// **Which voice gets the six is the unread part.** The PROM gives the multiset
/// and the slot order; mapping a slot to voice 0, 1 or 2 needs the register-RAM
/// address mapping or a measurement. WSG voice order is assumed here. If that
/// is ever established and disagrees, this is the line to rotate, and nothing
/// else changes: the spread is real even where the assignment is a guess.
const SLOT_DUTY: [f64; 3] = [5.0 / 16.0, 5.0 / 16.0, 6.0 / 16.0];

/// What separates one board in this family from another.
///
/// The volume network is a divider: the switched conductance the code selects
/// works against one fixed conductance, and the transfer is
/// `G / (G + bias_g)`. Every board here is that expression, and the reason
/// Galaga's is written `1 / (R_legs + 10k)` on its drawing is that dividing a
/// series pair through by `R_legs` gives the same thing. So a board is three
/// numbers, each of which is a part on a sheet.
#[derive(Clone, Copy, Debug)]
pub struct BoardParams {
    /// The divider's other arm, in siemens.
    pub bias_g: f64,
    /// The capacitor shunting the summing node, in farads. With the node's own
    /// resistance this is the low-pass whose corner moves with the volume code.
    pub shunt_c: f64,
    /// The coupling that removes the ladder's standing offset, as
    /// `(ohms, farads)`. Every board in the family has one somewhere; where the
    /// drawing does not settle its corner, a low one is used deliberately,
    /// because what it is there to do is remove a DC offset and not to shape
    /// anything in the band.
    pub coupling: (f64, f64),
    /// The 54XX explosion network summed at the same op-amp, on the boards that
    /// have one. `None` where the board has no 54XX at all.
    pub explosion: Option<ExplosionNetwork>,
}

/// The 54XX's three channels and how they reach the summing amplifier.
///
/// Transcribed in
/// [`namco-54xx-explosion.md`](../../docs/schematics/namco-54xx-explosion.md).
/// One constant serves Galaga and Xevious because their networks were read
/// separately and found identical, component for component. That is worth
/// stating rather than assuming: Galaga and Dig Dug share this board and do not
/// share a volume law one sheet away.
#[derive(Clone, Copy, Debug)]
pub struct ExplosionNetwork {
    /// The binary-weighted ladder on each channel's four output pins, MSB
    /// first. The same four values on every channel and both boards.
    pub ladder: [f64; 4],
    /// Per channel: the series resistor into the filter, the shunt to the
    /// reference, the feedback resistor, the filter capacitor, and the output
    /// leg into the summing node.
    pub channels: [ExplosionChannel; 3],
    /// What the op-amp's non-inverting inputs sit at, in volts. 3.3k over 2.2k
    /// off +5 V is about 2.0 V.
    pub reference: f64,
}

/// One explosion channel: a DAC, a multiple-feedback band-pass, and a leg into
/// the shared summing node.
#[derive(Clone, Copy, Debug)]
pub struct ExplosionChannel {
    pub series_ohms: f64,
    pub shunt_ohms: f64,
    pub feedback_ohms: f64,
    pub farads: f64,
    pub leg_ohms: f64,
}

impl ExplosionNetwork {
    /// Galaga's R21-R42 and Xevious's R104-R135, which are the same network.
    pub const NAMCO_54XX: Self = Self {
        ladder: [4_700.0, 10_000.0, 22_000.0, 47_000.0],
        channels: [
            ExplosionChannel {
                series_ohms: 100_000.0,
                shunt_ohms: 22_000.0,
                feedback_ohms: 220_000.0,
                farads: 1e-9,
                leg_ohms: 33_000.0,
            },
            ExplosionChannel {
                series_ohms: 47_000.0,
                shunt_ohms: 10_000.0,
                feedback_ohms: 150_000.0,
                farads: 10e-9,
                leg_ohms: 33_000.0,
            },
            ExplosionChannel {
                series_ohms: 150_000.0,
                shunt_ohms: 22_000.0,
                feedback_ohms: 470_000.0,
                farads: 10e-9,
                leg_ohms: 10_000.0,
            },
        ],
        reference: 2.0,
    };
}

/// The logic supply these boards run on.
///
/// The WSG side of this module works in fractions of that rail, because a DAC
/// switched between the rails is naturally unitless. The op-amp band-pass is
/// not: it clamps to real rails, offset below the positive one the way a real
/// single-supply part is, so the explosion path is built in volts and divided
/// back down where it joins the summing node.
const SUPPLY_V: f64 = 5.0;

/// The summing amplifier's feedback resistor, R20 on Galaga and R125 on
/// Xevious. Every leg's weight into the mix is this over the leg's own
/// resistance, so the WSG's 10k enters at 0.33 and a 33k explosion leg at 0.10.
const SUMMING_FEEDBACK: f64 = 3_300.0;

impl BoardParams {
    /// Pac-Man and Ms. Pac-Man. R96 22k in series with the 10k cabinet pot is
    /// the bias arm, C1 10 nF the shunt, C46 into R92 the coupling at 15.9 Hz.
    /// R5 is not in the bias: see `docs/schematics/pacman-audio-output.md`.
    pub const PACMAN: Self = Self {
        bias_g: 1.0 / 31_000.0,
        shunt_c: 10e-9,
        coupling: (100_000.0, 100e-9),
        explosion: None,
    };

    /// Galaga. R19 10k into the 5P LM324's virtual ground is the bias arm and
    /// C43 2.2 nF the shunt. The coupling is the 0.1 uF at VR1's wiper, whose
    /// corner needs the MB3730's input impedance and so is not on the drawing;
    /// 20 Hz stands in.
    pub const GALAGA: Self = Self {
        bias_g: 1.0 / 10_000.0,
        shunt_c: 2.2e-9,
        coupling: (100_000.0, 80e-9),
        explosion: Some(ExplosionNetwork::NAMCO_54XX),
    };

    /// Xevious, read 2026-09-17 and found to be Galaga's stage resistor for
    /// resistor: R119 10k into the 8A LM324, C7 0.0022 uF at the node. Filed
    /// separately from `GALAGA` because the two were established by two
    /// readings, and a shared board family predicts nothing.
    pub const XEVIOUS: Self = Self {
        bias_g: 1.0 / 10_000.0,
        shunt_c: 2.2e-9,
        coupling: (100_000.0, 80e-9),
        explosion: Some(ExplosionNetwork::NAMCO_54XX),
    };

    /// Dig Dug. R105 10k to +5 V and R108 10k to ground put a fixed 200 uS on
    /// the node, and C14 10 nF shunts it. C13 0.22 uF into R107 10k is the
    /// coupling, a 72 Hz corner, the one in the family the drawing does give.
    /// Dig Dug has the 51XX and 53XX where Galaga and Xevious have the 54XX, so
    /// there is no explosion network on it to model.
    pub const DIGDUG: Self = Self {
        bias_g: 200e-6,
        shunt_c: 10e-9,
        coupling: (10_000.0, 0.22e-6),
        explosion: None,
    };
}

/// The WSG's voices update at 96 kHz on this board, so the circuit has to run
/// above twice that to carry their steps rather than alias them. A floor, not
/// an exact rate: see [`DiscreteCircuitBuilder::with_sim_rate`], which rounds up
/// to a whole multiple of the resampler's intermediate rate.
///
/// The next multiple up, 352.8 kHz against a 44.1 kHz host, doubles the node
/// evaluations: 0.79 ms per frame becomes 1.08, which is 21x real time against
/// 15x. What that buys, measured over the committed movie, is a capture with the
/// same peak and AC RMS to 0.01 dB and no band moved by more than 0.02 pp. The
/// higher rate is paying for a difference nobody can hear.
const MIN_SIM_RATE: u64 = 176_400;

/// Where the path this replaces put its own full swing, as a fraction of the
/// `i16` rail. Kept so the board's loudness does not move when its timbre does.
///
/// `NamcoWsg::tick` scaled its summed `sample * volume` by 80, and three voices
/// at full sample and full volume come to 315, so that path saturated at 25200
/// of 32767. An ear judgment of the new stage is about the decay and the color;
/// it should not also be about the level.
const LEGACY_FULL_SCALE: f64 = 315.0 * 80.0 / 32767.0 * RECONSTRUCTION_HEADROOM;

/// Headroom for the overshoot the reconstruction filter adds, which neither
/// path's arithmetic predicts.
///
/// A stepped 4-bit wave through a windowed-sinc resampler rings past its own
/// steps, so a board measures hotter than its static full swing says. The
/// integer path did this too: its own arithmetic put full scale at 0.77 of the
/// rail and Dig Dug measured -0.91 dBFS, which is 0.90. Without an allowance
/// the stage inherits that and lands on the rail, where Dig Dug and Xevious
/// both measured -0.00 dBFS.
///
/// It is measured rather than chosen for taste. It was 0.8 while the voices
/// were summed, which put the family within about a decibel of the integer
/// multiply it replaced. **Modeling the multiplex invalidated that
/// calibration**, in two ways that both push the level up:
///
/// - Sparse passages got louder, because a voice playing alone is no longer a
///   third of three. That is the fix, not a side effect.
/// - The WSG node now carries its physical value rather than three times it,
///   so the 0.33 leg into the summing amplifier and the 54XX's own legs are
///   finally on a common footing. That moved the explosion path up by 9.55 dB
///   relative to the WSG, which is a 3x that had been hiding in the old
///   topology, and it is what put Galaga and Xevious on the rail.
///
/// 0.6 restores the margin. Peaks over the committed movies:
///
/// | machine | summed voices, 0.8 | multiplexed, 0.8 | multiplexed, 0.6 |
/// |---|---|---|---|
/// | pacman | -3.66 dBFS | -1.08 | -3.58 |
/// | mspacman | - | -1.95 | -4.45 |
/// | galaga | -4.07 | 0.00, clipping | -1.35 |
/// | digdug | -1.04 | -1.19 | -3.69 |
/// | xevious | -0.07 | -0.00, clipping | -1.35 |
///
/// Galaga and Xevious land higher than scaling their clipped column would
/// suggest, because a clipped peak does not say how far past the rail the
/// content went. Their 1.35 dB is the thinnest margin in the family and it is
/// still wider than the 0.07 dB Xevious used to run at.
///
/// The family comes out more uniform than it was, which is the multiplex
/// removing a voice-count dependence rather than a trim being applied: this is
/// still one factor for the whole family and not a per-board number nobody
/// could later tie to a part.
const RECONSTRUCTION_HEADROOM: f64 = 0.6;

// ---------------------------------------------------------------------------
// The two ladders, as arithmetic
// ---------------------------------------------------------------------------

/// Leg conductances, LSB first, which is the order a DAC's weights are given
/// in. The drawing lists the legs MSB first, so this reverses them.
fn conductances(legs: [f64; 4]) -> [f64; 4] {
    let mut g = [0.0; 4];
    for (bit, ohms) in legs.iter().rev().enumerate() {
        g[bit] = 1.0 / ohms;
    }
    g
}

/// Sample-ladder weights, LSB first, normalized so code 15 comes to 1.0.
///
/// Four legs switched between the latch's rails meeting at one node make the
/// node the conductance-weighted average of those rails, so a code is worth the
/// set legs' share of the total conductance.
fn sample_weights() -> [f64; 4] {
    let g = conductances(SAMPLE_LEGS);
    let total: f64 = g.iter().sum();
    [g[0] / total, g[1] / total, g[2] / total, g[3] / total]
}

/// What the summing node sits at for one sample code, as a fraction of the
/// latch's swing. Code 8 is **not** 0.5: that asymmetry is the point.
pub fn sample_level(code: u8) -> f64 {
    let w = sample_weights();
    (0..4).filter(|b| code & (1 << b) != 0).map(|b| w[b]).sum()
}

/// Conductance of the volume legs a code closes, in siemens.
fn volume_conductance(code: u8) -> f64 {
    let g = conductances(VOLUME_LEGS);
    (0..4).filter(|b| code & (1 << b) != 0).map(|b| g[b]).sum()
}

/// The volume network's gain for one code on one board: a divider against that
/// board's bias arm, so an open leg leaves the network rather than contributing
/// zero.
pub fn volume_gain(code: u8, params: BoardParams) -> f64 {
    let g = volume_conductance(code);
    if g == 0.0 {
        return 0.0;
    }
    g / (g + params.bias_g)
}

/// Where the shunt capacitor's corner sits for one volume code, in Hz. The code
/// chooses the node's resistance, so the corner is a function of the volume.
///
/// This is the **per-slot** figure, which is what the transcriptions compute
/// and what the drawing's Thevenin resistance gives: the corner while that one
/// voice holds the node. The corner the audio band actually sees is the
/// duty-weighted average over a frame, so it also depends on what the other two
/// voices are doing, and it only coincides with this when all three carry the
/// same code. One voice playing alone sits well below it; see [`SLOT_DUTY`].
pub fn corner_hz(code: u8, params: BoardParams) -> f64 {
    let g = volume_conductance(code) + params.bias_g;
    g / (std::f64::consts::TAU * params.shunt_c)
}

// ---------------------------------------------------------------------------
// C1
// ---------------------------------------------------------------------------

/// C1, whose corner follows whichever volume legs are closed.
///
/// Inputs are the summed bus and the three voices' closed conductance. The cap
/// sees all of them in parallel with the bus load, so the time constant has to
/// be recomputed per step rather than frozen at build time, which is why this
/// is a custom component and not an [`rc_low_pass`].
///
/// [`rc_low_pass`]: DiscreteCircuitBuilder::rc_low_pass
struct SwitchedCornerRc {
    farads: f64,
    load_g: f64,
    y: f64,
}

impl CustomComponent for SwitchedCornerRc {
    fn reset(&mut self) {
        self.y = 0.0;
    }

    fn step(&mut self, inputs: &[f64], dt: f64) -> f64 {
        let g = self.load_g + inputs[1..].iter().sum::<f64>();
        // `load_g` is a positive constant, so the total conductance is never
        // zero even with every voice silent and every 4066 section open.
        let tau = self.farads / g;
        let alpha = 1.0 - (-dt / tau).exp();
        self.y += alpha * (inputs[0] - self.y);
        self.y
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.write_f64_le(self.y);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.y = r.read_f64_le()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The circuit
// ---------------------------------------------------------------------------

/// One voice's two latch fields, as the board's inputs see them.
struct VoiceInputs {
    sample: DataInputId,
    volume: DataInputId,
}

struct Inputs {
    voices: [VoiceInputs; 3],
    sound: NodeId,
    /// The 54XX's three output ports, on the boards that have one.
    explosion: Option<[DataInputId; 3]>,
}

/// The explosion DAC's weights, LSB first, in volts: the full code drives the
/// filter's input to the supply rail. The same ladder on all three channels.
fn explosion_weights(net: &ExplosionNetwork) -> [f64; 4] {
    let g = conductances(net.ladder);
    let total: f64 = g.iter().sum();
    [
        SUPPLY_V * g[0] / total,
        SUPPLY_V * g[1] / total,
        SUPPLY_V * g[2] / total,
        SUPPLY_V * g[3] / total,
    ]
}

fn build_circuit(params: BoardParams, board_clock_hz: u64) -> (DiscreteCircuit, Inputs) {
    let mut b = DiscreteCircuitBuilder::new(board_clock_hz, host_sample_rate() as u64)
        .with_sim_rate(MIN_SIM_RATE);

    let weights = sample_weights();
    // Every voice's legs land on one node, because the board has one network.
    let mut taps: Vec<(NodeId, f64, Option<NodeId>)> = Vec::with_capacity(12);
    let mut corner_inputs = Vec::with_capacity(3);
    let mut voices = Vec::with_capacity(3);

    for (v, &duty) in SLOT_DUTY.iter().enumerate() {
        // The latch's two fields. The sample field is unsigned here, exactly as
        // 5Q-8Q are: the DC it carries is C46's to remove, further down.
        let sample = b.data_input(&format!("SAMPLE{v}"), 1.0);
        let volume = b.data_input(&format!("VOL{v}"), 1.0);

        let node = b.dac_weighted(&format!("DAC{v}"), sample, &weights);

        // The four 4066 sections tap that one node. Gating the sources instead
        // would leave every resistor in the divider, which is the error
        // `resistor_mixer_switched` exists to prevent.
        //
        // A leg is only in circuit for this voice's slots, so over a frame it
        // averages `duty / ohms` of conductance. Scaling the resistance is what
        // turns three networks that would each fight the bias arm on their own
        // into the one network the board has.
        for (i, ohms) in VOLUME_LEGS.iter().enumerate() {
            let bit = 3 - i as u8;
            let sw = b.bit_decode(&format!("VOL{v}_B{bit}"), volume, bit);
            taps.push((node, ohms / duty, Some(sw)));
        }

        // What C1 sees through this voice's closed legs, on the same average.
        let g: Vec<f64> = conductances(VOLUME_LEGS).iter().map(|g| g * duty).collect();
        corner_inputs.push(b.dac_weighted(&format!("VOL{v}_G"), volume, &g));

        voices.push(VoiceInputs { sample, volume });
    }

    // The shared node, and the bias arm that is never switched out.
    let sound = b.resistor_mixer_switched("SOUND", &taps, Some(1.0 / params.bias_g));
    let mut c1_inputs = vec![sound];
    c1_inputs.extend(corner_inputs);
    let filtered = b.custom(
        "SHUNT",
        c1_inputs,
        Box::new(SwitchedCornerRc {
            farads: params.shunt_c,
            load_g: params.bias_g,
            y: 0.0,
        }),
    );

    // The summing amplifier, on the boards that have one. Its inverting node
    // takes the WSG through the same resistor that is the WSG divider's bias
    // arm, and the 54XX's three filter legs alongside it, so each source enters
    // at the feedback over its own leg: 0.33 for the WSG's 10k, 0.10 for a 33k
    // explosion leg. See `docs/schematics/namco-54xx-explosion.md`, and note
    // that this junction belongs to neither row alone.
    let (mixed, explosion) = match params.explosion {
        None => (filtered, None),
        Some(net) => {
            let wsg_leg = b.gain("WSG_LEG", filtered, SUMMING_FEEDBACK * params.bias_g);
            let mut legs = vec![wsg_leg];
            let mut inputs = Vec::with_capacity(3);
            for (i, ch) in net.channels.iter().enumerate() {
                // The MCU's four output pins, as one code this end.
                let code = b.data_input(&format!("EXPL{i}"), 1.0);
                let dac = b.dac_weighted(&format!("EXPL{i}_DAC"), code, &explosion_weights(&net));
                // A multiple-feedback band-pass: the series resistor carries the
                // signal and the shunt sits to the reference, which is where
                // this single-supply op-amp's inputs are held.
                let bp = b.op_amp_band_pass(
                    &format!("EXPL{i}_BP"),
                    dac,
                    &[ch.series_ohms, ch.shunt_ohms],
                    ch.feedback_ohms,
                    ch.farads,
                    ch.farads,
                    net.reference,
                    0.0,
                    SUPPLY_V,
                );
                // Back to fractions of the rail, which is what the WSG side of
                // this circuit is in.
                legs.push(b.gain(
                    &format!("EXPL{i}_LEG"),
                    bp,
                    SUMMING_FEEDBACK / ch.leg_ohms / SUPPLY_V,
                ));
                inputs.push(code);
            }
            let summed = b.add("SUM", &legs);
            (summed, Some([inputs[0], inputs[1], inputs[2]]))
        }
    };

    // The coupling, and what removes the sample ladder's standing offset.
    let (r, c) = params.coupling;
    let coupled = b.rc_high_pass("COUPLING", mixed, r, c);

    b.output(coupled, OutputGain::linear(output_gain(params)));

    let circuit = b.build();
    (
        circuit,
        Inputs {
            voices: [voices.remove(0), voices.remove(0), voices.remove(0)],
            sound,
            explosion,
        },
    )
}

/// Scale the stage's own full swing onto the one the integer path produced.
///
/// Full swing is three voices at full volume, each at whichever half of the
/// sample ladder is the larger: the negative one, since the zero sits above
/// mid-scale.
///
/// [`OutputGain`] normalizes to +/-1 before it reaches the rail, so this is a
/// fraction of full scale and not an `i16`.
fn output_gain(params: BoardParams) -> f64 {
    LEGACY_FULL_SCALE / full_swing(params)
}

/// The largest excursion the stage can present to [`OutputGain`], in circuit
/// units: every voice at full volume, swinging the larger half of the sample
/// ladder.
///
/// There is no voice count in this. [`SLOT_DUTY`] sums to one, so three voices
/// at full volume drive the node exactly as one voice holding it for the whole
/// frame would, and the board's loudest state is one code-15 divider rather
/// than three. That is what keeps [`LEGACY_FULL_SCALE`] meaning the same thing
/// across the topology change, and it is also why the duty split can be
/// rotated without moving the family's level.
fn full_swing(params: BoardParams) -> f64 {
    let zero = sample_level(8);
    let wsg = zero.max(1.0 - zero) * volume_gain(15, params);
    match params.explosion {
        // The summing amplifier scales the WSG by its own leg, so full scale
        // has to be measured after it or the boards with one come out quiet by
        // exactly that factor.
        Some(_) => wsg * SUMMING_FEEDBACK * params.bias_g,
        None => wsg,
    }
}

// ---------------------------------------------------------------------------
// Board-facing wrapper
// ---------------------------------------------------------------------------

/// One board's WSG output stage: the two ladders, the shunt and the coupling.
#[derive(Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct WsgOutputStage {
    #[save(id = 1)]
    circuit: DiscreteCircuit,
    /// Input handles, fixed when the circuit is built.
    #[save_skip]
    ids: Inputs,
}

impl WsgOutputStage {
    /// Build the stage for one board, driven by a `board_clock_hz` clock.
    pub fn new(params: BoardParams, board_clock_hz: u64) -> Self {
        let (circuit, ids) = build_circuit(params, board_clock_hz);
        Self { circuit, ids }
    }

    /// Latch one board cycle's worth of voice codes and advance the stage.
    ///
    /// `voices` is [`NamcoWsg::tick_voices`]'s result: each voice's signed
    /// waveform sample and its volume code. The sample goes back to the
    /// unsigned nibble the latch actually holds, because the ladder is driven
    /// by latch outputs and its zero is wherever code 8 lands.
    ///
    /// [`NamcoWsg::tick_voices`]: phosphor_core::device::namco_wsg::NamcoWsg::tick_voices
    /// Every code is pushed every cycle rather than only on a change. A cache
    /// of the last codes would be state the circuit's own inputs do not carry
    /// across a save: two instances with different histories would then make
    /// different decisions about what to refresh, and the save-state round trip
    /// is what caught that.
    pub fn tick(&mut self, voices: [(i32, u8); 3]) {
        for (v, &(sample, volume)) in voices.iter().enumerate() {
            let code = (sample + 8).clamp(0, 15) as f64;
            self.circuit.set_data(self.ids.voices[v].sample, code);
            self.circuit
                .set_data(self.ids.voices[v].volume, volume as f64);
        }
        self.circuit.tick(1);
    }

    /// Latch the 54XX's three output ports, each a four-bit code.
    ///
    /// A no-op on a board with no 54XX, which is Pac-Man, Ms. Pac-Man and Dig
    /// Dug: only Galaga and Xevious have an explosion network to drive.
    ///
    /// The codes come from [`Namco54Lle::channels`] in ladder order, so `ports`
    /// is the 100k leg first, then the 47k, then the 150k, matching
    /// [`ExplosionNetwork::channels`]. Note that on Galaga's self-test
    /// explosion the 100k channel is never driven: only the two O-port channels
    /// move, so a change to the 100k path will not show there.
    ///
    /// [`Namco54Lle::channels`]: phosphor_core::device::namco54::Namco54Lle::channels
    pub fn set_explosion(&mut self, ports: [u8; 3]) {
        let Some(ids) = self.ids.explosion else {
            return;
        };
        for (id, code) in ids.iter().zip(ports) {
            self.circuit.set_data(*id, (code & 0x0F) as f64);
        }
    }

    /// Drain produced mono `i16` samples. Returns the number written.
    pub fn fill_audio(&mut self, out: &mut [i16]) -> usize {
        self.circuit.fill_audio(out)
    }

    /// The built circuit, so a probe can render one named node.
    pub fn circuit(&self) -> &DiscreteCircuit {
        &self.circuit
    }

    /// The summing bus, ahead of C1 and the coupling.
    pub fn sound_node(&self) -> NodeId {
        self.ids.sound
    }

    pub fn reset(&mut self) {
        self.circuit.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::namco_pac::TIMING;

    /// The board most of these assert against. Pac-Man, because its numbers are
    /// the ones written up in its transcription and because it is the board this
    /// stage was first built for.
    const P: BoardParams = BoardParams::PACMAN;

    fn db(ratio: f64) -> f64 {
        20.0 * ratio.log10()
    }

    /// The ladder is the conductance share of the legs a code closes, and the
    /// two ends of it are what the whole reading turns on.
    #[test]
    fn the_sample_ladder_spans_zero_to_one() {
        assert_eq!(sample_level(0), 0.0);
        assert!((sample_level(15) - 1.0).abs() < 1e-12);
    }

    /// The finding the transcription calls the point of the reading: the
    /// waveform's zero does not land at half scale, so the negative half-swing
    /// is the larger one and what that adds is even harmonics.
    #[test]
    fn the_sample_ladders_zero_sits_above_mid_scale() {
        let zero = sample_level(8);
        assert!(
            (zero - 0.5607).abs() < 5e-4,
            "code 8 should sit at 0.5607 of full scale, got {zero}"
        );
        let asymmetry = zero / (1.0 - zero);
        assert!(
            (db(asymmetry) - 2.12).abs() < 0.05,
            "the negative half should be 2.12 dB larger, got {:.2}",
            db(asymmetry)
        );
    }

    /// The compressed ends the drawing's two ratio tables record. Neither
    /// ladder is binary, and both are compressed at the bottom in the same way.
    #[test]
    fn neither_ladder_is_an_exact_binary_one() {
        let w = sample_weights();
        // Normalized to the largest leg: 1 : 0.470 : 0.214 : 0.100.
        assert!((w[2] / w[3] - 0.470).abs() < 5e-3, "{:?}", w);
        assert!((w[1] / w[3] - 0.214).abs() < 5e-3, "{:?}", w);
        assert!((w[0] / w[3] - 0.100).abs() < 5e-3, "{:?}", w);

        let g = conductances(VOLUME_LEGS);
        assert!((g[2] / g[3] - 0.455).abs() < 5e-3, "{:?}", g);
        assert!((g[1] / g[3] - 0.213).abs() < 5e-3, "{:?}", g);
        assert!((g[0] / g[3] - 0.100).abs() < 5e-3, "{:?}", g);
    }

    /// The large mechanism, and the reason this stage exists: the volume
    /// network is a divider, so its law is far more compressed than the
    /// integer multiply it replaces. These are the numbers in the
    /// transcription's table.
    #[test]
    fn the_volume_law_is_compressed_against_an_integer_multiply() {
        let full = volume_gain(15, P);
        for (code, want) in [(1u8, -11.1), (2, -6.6), (4, -3.2), (8, -1.0)] {
            let got = db(volume_gain(code, P) / full);
            assert!(
                (got - want).abs() < 0.1,
                "volume {code}: board should be {want} dB down, got {got:.1}"
            );
            let integer = db(code as f64 / 15.0);
            assert!(
                got > integer + 4.0,
                "volume {code}: the board must sit well above the integer \
                 multiply's {integer:.1} dB, got {got:.1}"
            );
        }
        assert_eq!(volume_gain(0, P), 0.0, "every section open is silence");
    }

    /// Opening a 4066 section removes its resistor, so the legs that are left
    /// get louder. A model that gated the sources instead would make the gain
    /// fall monotonically with the count of closed legs, and this is the
    /// property that separates the two.
    #[test]
    fn a_single_leg_is_louder_than_its_share_of_the_whole() {
        // Code 8 closes only the 10k leg, which is 1 of 4 legs but carries
        // over half the network's conductance and 89 % of its gain.
        let share = volume_gain(8, P) / volume_gain(15, P);
        assert!(
            share > 0.85,
            "one leg of four should be most of it: {share}"
        );
    }

    /// The shunt's corner follows the closed legs. The ends of that range are
    /// what makes a note get darker as it decays, and every board's pair is a
    /// number its own transcription states.
    #[test]
    fn the_filter_corner_moves_with_the_volume_code() {
        for (what, params, at_1, at_15) in [
            ("pacman", BoardParams::PACMAN, 673.0, 3326.0),
            ("digdug", BoardParams::DIGDUG, 3300.0, 6000.0),
            ("galaga", BoardParams::GALAGA, 8000.0, 20_000.0),
            ("xevious", BoardParams::XEVIOUS, 8000.0, 20_000.0),
        ] {
            let lo = corner_hz(1, params);
            let hi = corner_hz(15, params);
            assert!(
                (lo - at_1).abs() < at_1 * 0.02,
                "{what} code 1 corner {lo:.0}, expected about {at_1:.0}"
            );
            assert!(
                (hi - at_15).abs() < at_15 * 0.02,
                "{what} code 15 corner {hi:.0}, expected about {at_15:.0}"
            );
            assert!(hi > lo, "{what}: quieter must be darker");
        }
    }

    /// The family is one law with one number changed, and that number is the
    /// only thing separating these boards' volume curves. The two figures are
    /// the ones in the Galaga transcription's table.
    #[test]
    fn the_boards_differ_only_in_the_bias_arm() {
        let vs_linear = |code: u8, p: BoardParams| {
            db((volume_gain(code, p) / volume_gain(15, p)) / (code as f64 / 15.0))
        };
        assert!(
            (vs_linear(1, BoardParams::DIGDUG) - 3.65).abs() < 0.05,
            "dig dug at code 1: {:.2}",
            vs_linear(1, BoardParams::DIGDUG)
        );
        assert!(
            (vs_linear(1, BoardParams::GALAGA) - 6.59).abs() < 0.05,
            "galaga at code 1: {:.2}",
            vs_linear(1, BoardParams::GALAGA)
        );
        // Xevious was read, not assumed, and what the reading found was
        // Galaga's stage. If that ever stops being true here, it is because
        // someone changed a constant rather than because a board changed.
        assert_eq!(
            BoardParams::XEVIOUS.bias_g,
            BoardParams::GALAGA.bias_g,
            "xevious's R119 10k is Galaga's R19 10k"
        );
        // Every board is compressed against the integer multiply, and more so
        // the quieter the code. That is the mechanism, not a per-board quirk.
        for p in [
            BoardParams::PACMAN,
            BoardParams::GALAGA,
            BoardParams::XEVIOUS,
            BoardParams::DIGDUG,
        ] {
            assert!(vs_linear(1, p) > vs_linear(8, p), "{:?}", p);
            assert!(vs_linear(8, p) > 0.0, "{:?}", p);
        }
    }

    /// Only Galaga and Xevious have a 54XX. Pac-Man's family and Dig Dug have
    /// no explosion network to sum, and driving one at them must do nothing
    /// rather than quietly build a circuit they do not have.
    #[test]
    fn only_the_boards_with_a_54xx_carry_an_explosion_network() {
        assert!(BoardParams::GALAGA.explosion.is_some());
        assert!(BoardParams::XEVIOUS.explosion.is_some());
        assert!(BoardParams::PACMAN.explosion.is_none());
        assert!(BoardParams::DIGDUG.explosion.is_none());

        // Driving the ports on a board without one is a no-op, not a panic.
        let mut out = WsgOutputStage::new(BoardParams::PACMAN, TIMING.cpu_clock_hz);
        out.set_explosion([15, 15, 15]);
        let mut buf = [0i16; 512];
        for _ in 0..20_000 {
            out.tick([(0, 0); 3]);
            out.fill_audio(&mut buf);
        }
        assert!(
            buf.iter().all(|s| *s == 0),
            "a board with no 54XX must stay silent when one is driven at it"
        );
    }

    /// The two networks were read separately and found identical, so they are
    /// one constant. If a later reading ever splits them, this is the assert
    /// that should be deleted rather than edited around.
    #[test]
    fn galaga_and_xevious_share_one_explosion_network() {
        let g = BoardParams::GALAGA.explosion.unwrap();
        let x = BoardParams::XEVIOUS.explosion.unwrap();
        assert_eq!(g.ladder, x.ladder);
        assert_eq!(g.reference, x.reference);
        for (a, b) in g.channels.iter().zip(&x.channels) {
            assert_eq!(a.series_ohms, b.series_ohms);
            assert_eq!(a.shunt_ohms, b.shunt_ohms);
            assert_eq!(a.feedback_ohms, b.feedback_ohms);
            assert_eq!(a.farads, b.farads);
            assert_eq!(a.leg_ohms, b.leg_ohms);
        }
    }

    /// What each source is worth at the summing node, which is the number that
    /// decides how loud an explosion is against the music. Channel 3's leg is
    /// 10k where the other two are 33k, so it enters three times louder.
    #[test]
    fn the_summing_legs_carry_the_weights_the_drawing_gives() {
        let net = BoardParams::GALAGA.explosion.unwrap();
        let weight = |ohms: f64| SUMMING_FEEDBACK / ohms;
        // The WSG's own leg is the same resistor as its divider's bias arm.
        assert!((weight(1.0 / BoardParams::GALAGA.bias_g) - 0.33).abs() < 0.005);
        assert!((weight(net.channels[0].leg_ohms) - 0.10) < 0.005);
        assert!((weight(net.channels[1].leg_ohms) - 0.10) < 0.005);
        assert!(
            (weight(net.channels[2].leg_ohms) - 0.33).abs() < 0.005,
            "channel 3's 10k leg should match the WSG's own"
        );
    }

    /// The three band-passes are an order of magnitude apart, which is what
    /// makes them three voices rather than one with ripple. Centers from
    /// `1/(2*pi*sqrt(r_total*rf*c1*c2))` on the transcribed values.
    #[test]
    fn the_three_filters_sit_an_octave_decade_apart() {
        let net = BoardParams::GALAGA.explosion.unwrap();
        let center = |c: &ExplosionChannel| {
            let r_total = 1.0 / (1.0 / c.series_ohms + 1.0 / c.shunt_ohms);
            1.0 / (std::f64::consts::TAU * (r_total * c.feedback_ohms).sqrt() * c.farads)
        };
        let f: Vec<f64> = net.channels.iter().map(center).collect();
        assert!(
            f[0] > 5.0 * f[1],
            "the 0.001 uF section should sit far above the others: {f:?}"
        );
        assert!(f[1] > f[2], "and channel 2 above channel 3: {f:?}");
    }

    /// Dig Dug's node passes less than half the sample swing at full volume,
    /// because 176.7 uS of legs works against 200 uS of bias. That is the
    /// divider rather than a loss to trim out, and it is the clearest case of
    /// why the bias arm cannot be normalized away.
    #[test]
    fn dig_dugs_bias_costs_it_half_the_swing() {
        let g = volume_gain(15, BoardParams::DIGDUG);
        assert!((g - 0.469).abs() < 0.005, "{g}");
        assert!(volume_gain(15, BoardParams::GALAGA) > 0.6);
    }

    /// Silence in, silence out, and no panic from a zero conductance: with
    /// every section open the cap still sees the bus load.
    #[test]
    fn silence_produces_silence() {
        let mut out = WsgOutputStage::new(P, TIMING.cpu_clock_hz);
        for _ in 0..10_000 {
            out.tick([(0, 0); 3]);
        }
        let mut buf = [0i16; 256];
        let n = out.fill_audio(&mut buf);
        assert!(n > 0, "the stage should still produce samples");
        assert!(
            buf[..n].iter().all(|s| *s == 0),
            "silent voices must leave the output at zero"
        );
    }

    /// A held sample code is a DC level, and C46 is what takes it out. Without
    /// the coupling the sample ladder's standing offset would be a permanent
    /// rail on every note.
    #[test]
    fn a_held_code_decays_away_through_the_coupling() {
        let mut out = WsgOutputStage::new(P, TIMING.cpu_clock_hz);
        let mut buf = [0i16; 4096];
        let mut samples = Vec::new();
        // A quarter second of the same latch contents.
        for _ in 0..(TIMING.cpu_clock_hz / 4) {
            out.tick([(7, 15), (0, 0), (0, 0)]);
            let n = out.fill_audio(&mut buf);
            samples.extend_from_slice(&buf[..n]);
        }
        assert!(samples.len() > 1_000, "no audio: {}", samples.len());

        // The resampler's window takes a moment to fill, so the step is judged
        // over the first tenth of the run rather than at the first sample.
        let window = samples.len() / 10;
        let peak = |s: &[i16]| s.iter().map(|v| v.unsigned_abs()).max().unwrap();
        let started = peak(&samples[..window]);
        let ended = peak(&samples[samples.len() - window..]);
        assert!(
            started > 1_000,
            "the step should reach the output: {started}"
        );
        assert!(
            ended < started / 10,
            "a held code should decay through C46: started {started}, ended {ended}"
        );
    }

    /// The whole reason the codes arrive separately: a quiet note through this
    /// stage is much louder than the integer product makes it, and the gap is
    /// the volume law's.
    #[test]
    fn a_quiet_note_is_louder_here_than_an_integer_multiply_makes_it() {
        let loud = sample_level(15) * volume_gain(15, P);
        let quiet = sample_level(15) * volume_gain(1, P);
        let board = db(quiet / loud);
        let integer = db((15.0 * 1.0) / (15.0 * 15.0));
        assert!(
            board - integer > 10.0,
            "volume 1 should sit {board:.1} dB down where the integer multiply \
             puts it at {integer:.1} dB"
        );
    }

    /// The loudest thing the WSG can present is all three voices at full
    /// volume swinging the whole waveform, and it has to fit an `i16` with the
    /// margin real content needs. A theoretical peak is not enough here: the
    /// coupling passes a step whole, so what the stage does with a moving
    /// signal is what decides the headroom.
    #[test]
    fn the_loudest_drive_stays_off_the_rail() {
        let mut out = WsgOutputStage::new(P, TIMING.cpu_clock_hz);
        let mut buf = [0i16; 4096];
        let mut samples = Vec::new();
        // A 1 kHz full-swing square on every voice at full volume.
        let half_period = TIMING.cpu_clock_hz / 2_000;
        for cycle in 0..(TIMING.cpu_clock_hz / 4) {
            let sample = if (cycle / half_period).is_multiple_of(2) {
                7
            } else {
                -8
            };
            out.tick([(sample, 15); 3]);
            let n = out.fill_audio(&mut buf);
            samples.extend_from_slice(&buf[..n]);
        }
        // After the coupling has settled: the power-on step through C46 is a
        // transient the real board also has, and it is not what sets the level
        // a game plays at.
        let settled = &samples[samples.len() / 5..];
        let peak = settled
            .iter()
            .map(|s| s.unsigned_abs())
            .max()
            .expect("no audio");
        assert!(
            peak < 32_000,
            "the loudest drive should stay off the rail, peaked at {peak}"
        );
    }

    /// The sequencer PROM's reading, as arithmetic. The 74LS273 holds until it
    /// is next clocked, so the three gaps between the strobes have to account
    /// for the whole frame; a set that did not sum to one would mean a slot
    /// where the DAC drives nothing, which the part cannot do.
    #[test]
    fn the_sequencer_proms_slots_account_for_the_whole_frame() {
        let total: f64 = SLOT_DUTY.iter().sum();
        assert!(
            (total - 1.0).abs() < 1e-12,
            "the latch holds, so the slots must cover the frame: {total}"
        );

        let mut slots: Vec<f64> = SLOT_DUTY.iter().map(|d| d * 16.0).collect();
        slots.sort_by(|a, b| a.partial_cmp(b).expect("no NaN"));
        assert_eq!(slots, vec![5.0, 5.0, 6.0], "strobes at slots 5, 10 and 15");

        // The whole point of reading the PROM rather than assuming thirds.
        let spread = db(slots[2] / slots[0]);
        assert!(
            (spread - 1.58).abs() < 0.01,
            "the long voice should sit 1.58 dB above the short ones, got {spread:.2}"
        );
    }

    /// The mix is compressive, which is the mechanism the shared network adds.
    ///
    /// One voice holds the node for its own slots against a bias arm that is
    /// never switched out, so it is worth far more than a third of three
    /// voices. A model that summed three independent dividers puts it at
    /// exactly a third, and that is the error this topology exists to remove:
    /// on Pac-Man it was 7 dB on every sparse passage.
    #[test]
    fn a_voice_playing_alone_is_worth_more_than_a_third_of_three() {
        let peak = |active: usize| {
            let mut out = WsgOutputStage::new(P, TIMING.cpu_clock_hz);
            let mut buf = [0i16; 4096];
            let mut samples = Vec::new();
            let half_period = TIMING.cpu_clock_hz / 2_000;
            for cycle in 0..(TIMING.cpu_clock_hz / 4) {
                let s = if (cycle / half_period).is_multiple_of(2) {
                    7
                } else {
                    -8
                };
                let mut voices = [(0i32, 0u8); 3];
                for slot in voices.iter_mut().take(active) {
                    *slot = (s, 15);
                }
                out.tick(voices);
                let n = out.fill_audio(&mut buf);
                samples.extend_from_slice(&buf[..n]);
            }
            let settled = &samples[samples.len() / 5..];
            f64::from(
                settled
                    .iter()
                    .map(|s| s.unsigned_abs())
                    .max()
                    .expect("no audio"),
            )
        };

        let share = peak(1) / peak(3);
        assert!(
            share > 0.6,
            "a lone voice should be most of the node, not a third: {share:.3}"
        );
        assert!(
            share < 1.0,
            "but still below three voices, which is full scale: {share:.3}"
        );
    }

    /// Full scale lands where the integer path's did, so an ear judgment of
    /// this stage is about its decay and its color and not about its level.
    #[test]
    fn full_scale_matches_the_path_this_replaces() {
        let peak = full_swing(P) * output_gain(P);
        assert!(
            (peak - LEGACY_FULL_SCALE).abs() < 1e-9,
            "full swing should render at {LEGACY_FULL_SCALE} of the rail, got {peak}"
        );
        // `OutputGain` clamps to +/-1, so a fraction at or above 1.0 is a stage
        // that saturates before a game has played anything.
        assert!(peak < 0.9, "full swing must leave headroom: {peak}");
    }
}
