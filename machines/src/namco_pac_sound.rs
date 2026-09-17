//! Pac-Man's analog audio output stage, built on the [`DiscreteCircuit`]
//! runtime.
//!
//! Transcribed in
//! [`docs/schematics/pacman-audio-output.md`](../../docs/schematics/pacman-audio-output.md),
//! read from the `PAC-MAN` game logic schematic at PDF p23. Tracked as
//! `phosphor-emulator-ga9p`.
//!
//! # What this board does that the WSG does not
//!
//! The chip's four-bit sample and four-bit volume leave the 74LS273 at 2M as
//! eight separate lines, and the board multiplies them in the analog domain:
//! 5Q-8Q drive R9 470, R8 1k, R7 2.2k and R6 4.7k into one summing node, and
//! 1Q-4Q gate four 4066 sections that tap that node through R2 10k, R1 22k,
//! R3 47k and R4 100k. So the product `sample * volume` that
//! [`NamcoWsg::tick`](phosphor_core::device::namco_wsg::NamcoWsg::tick) forms
//! is the one quantity this board never has, which is why the voices arrive
//! here as codes through
//! [`tick_voices`](phosphor_core::device::namco_wsg::NamcoWsg::tick_voices).
//!
//! Three mechanisms come out of the resistor values, and the arithmetic behind
//! each is in the transcription:
//!
//! - **The volume network is a divider**, not a sum of leg gains. A leg that is
//!   switched out leaves the network, so the gain is
//!   `load / (R_parallel + load)` and the law is strongly compressed: code 1
//!   sits 11.1 dB below full scale where `code / 15` puts it at 23.5 dB. Every
//!   decaying note decays about twice as far in dB with the multiply done in
//!   integers.
//! - **The sample ladder is asymmetric.** Its legs are switched between the
//!   latch's rails, so code 8, the waveform's zero, lands at 0.5607 of full
//!   scale rather than 0.5. The negative half-swing is 27.6 % larger than the
//!   positive one and what that adds is even harmonics, not gain.
//! - **C1's corner moves with the volume code**, because the code decides which
//!   resistors are in circuit: 673 Hz at code 1 against 3.3 kHz at code 15.
//!   A note gets darker as it decays as well as quieter.
//!
//! # The load, and what is assumed
//!
//! [`BUS_LOAD`] is the one number here that the drawing does not settle. R5, a
//! 22k trimmer at the output bus, was never traced, and it sets how compressed
//! the volume law is: 31k gives a 6.1 dB RMS departure from the integer
//! multiply, 12.9k gives 4.2 dB. **Only what was traced is modeled.** The
//! direction and rough size hold either way, which is what makes this worth
//! building before R5 is resolved.
//!
//! # Where this departs from the board
//!
//! The real board time-multiplexes one latch, one DAC and one switched network
//! across the three voices, so only one voice's legs are in circuit at a time.
//! Here the three voices have a network each and are summed, because that is
//! the shape the WSG presents. C1 is still one capacitor: its corner is set
//! from the conductance of every closed leg, which is what the shared node
//! sees. What this cannot show is the per-slot corner stepping at the
//! multiplex rate, which is above the audio band in any case.

use phosphor_core::audio::host_sample_rate;
use phosphor_core::core::save_state::{SaveError, StateReader, StateWriter};
use phosphor_core::device::discrete::{
    CustomComponent, DataInputId, DiscreteCircuit, DiscreteCircuitBuilder, NodeId, OutputGain,
};
use phosphor_macros::Saveable;

use crate::namco_pac::TIMING;

// ---------------------------------------------------------------------------
// The drawing
// ---------------------------------------------------------------------------

/// R9, R8, R7, R6: the sample ladder, MSB (latch 5Q) first.
const SAMPLE_LEGS: [f64; 4] = [470.0, 1_000.0, 2_200.0, 4_700.0];
/// R2, R1, R3, R4: the 4066-switched volume legs, MSB (latch 1Q) first.
const VOLUME_LEGS: [f64; 4] = [10_000.0, 22_000.0, 47_000.0, 100_000.0];
/// C1 at the summing output.
const C1: f64 = 10e-9;
/// R96 22k in series with the 10k cabinet pot, the load the volume legs drive.
/// See the module header: R5 is not in this and was never traced.
const BUS_LOAD: f64 = 31_000.0;
/// C46 into R92, the coupling ahead of the LM1877. A 15.9 Hz corner.
const C46: f64 = 100e-9;
const R92: f64 = 100_000.0;

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
const LEGACY_FULL_SCALE: f64 = 315.0 * 80.0 / 32767.0;

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

/// The volume network's gain for one code: a divider into [`BUS_LOAD`], so an
/// open leg leaves the network rather than contributing zero.
pub fn volume_gain(code: u8) -> f64 {
    let g = volume_conductance(code);
    if g == 0.0 {
        return 0.0;
    }
    g / (g + 1.0 / BUS_LOAD)
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
}

fn build_circuit() -> (DiscreteCircuit, Inputs) {
    let mut b = DiscreteCircuitBuilder::new(TIMING.cpu_clock_hz, host_sample_rate() as u64)
        .with_sim_rate(MIN_SIM_RATE);

    let weights = sample_weights();
    let mut legs = Vec::with_capacity(3);
    let mut corner_inputs = Vec::with_capacity(4);
    let mut voices = Vec::with_capacity(3);

    for v in 0..3 {
        // The latch's two fields. The sample field is unsigned here, exactly as
        // 5Q-8Q are: the DC it carries is C46's to remove, further down.
        let sample = b.data_input(&format!("SAMPLE{v}"), 1.0);
        let volume = b.data_input(&format!("VOL{v}"), 1.0);

        let node = b.dac_weighted(&format!("DAC{v}"), sample, &weights);

        // The four 4066 sections tap that one node. Gating the sources instead
        // would leave every resistor in the divider, which is the error
        // `resistor_mixer_switched` exists to prevent.
        let taps: Vec<(NodeId, f64, Option<NodeId>)> = VOLUME_LEGS
            .iter()
            .enumerate()
            .map(|(i, ohms)| {
                let bit = 3 - i as u8;
                let sw = b.bit_decode(&format!("VOL{v}_B{bit}"), volume, bit);
                (node, *ohms, Some(sw))
            })
            .collect();
        legs.push(b.resistor_mixer_switched(&format!("LEG{v}"), &taps, Some(BUS_LOAD)));

        // What C1 sees through this voice's closed legs.
        corner_inputs.push(b.dac_weighted(
            &format!("VOL{v}_G"),
            volume,
            &conductances(VOLUME_LEGS),
        ));

        voices.push(VoiceInputs { sample, volume });
    }

    let sound = b.add("SOUND", &legs);
    let mut c1_inputs = vec![sound];
    c1_inputs.extend(corner_inputs);
    let filtered = b.custom(
        "C1",
        c1_inputs,
        Box::new(SwitchedCornerRc {
            farads: C1,
            load_g: 1.0 / BUS_LOAD,
            y: 0.0,
        }),
    );

    // C46 into R92: the wiper's coupling, and what removes the sample ladder's
    // standing offset.
    let coupled = b.rc_high_pass("C46", filtered, R92, C46);

    b.output(coupled, OutputGain::linear(output_gain()));

    let circuit = b.build();
    (
        circuit,
        Inputs {
            voices: [voices.remove(0), voices.remove(0), voices.remove(0)],
            sound,
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
fn output_gain() -> f64 {
    LEGACY_FULL_SCALE / full_swing()
}

/// The largest excursion the stage can present to [`OutputGain`], in circuit
/// units: three voices at full volume, each swinging the larger half of the
/// sample ladder.
fn full_swing() -> f64 {
    let zero = sample_level(8);
    3.0 * zero.max(1.0 - zero) * volume_gain(15)
}

// ---------------------------------------------------------------------------
// Board-facing wrapper
// ---------------------------------------------------------------------------

/// Pac-Man's output stage: the two ladders, C1 and the coupling.
#[derive(Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct PacmanAudioOutput {
    #[save(id = 1)]
    circuit: DiscreteCircuit,
    /// Input handles, fixed when the circuit is built.
    #[save_skip]
    ids: Inputs,
}

impl PacmanAudioOutput {
    pub fn new() -> Self {
        let (circuit, ids) = build_circuit();
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

impl Default for PacmanAudioOutput {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let full = volume_gain(15);
        for (code, want) in [(1u8, -11.1), (2, -6.6), (4, -3.2), (8, -1.0)] {
            let got = db(volume_gain(code) / full);
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
        assert_eq!(volume_gain(0), 0.0, "every section open is silence");
    }

    /// Opening a 4066 section removes its resistor, so the legs that are left
    /// get louder. A model that gated the sources instead would make the gain
    /// fall monotonically with the count of closed legs, and this is the
    /// property that separates the two.
    #[test]
    fn a_single_leg_is_louder_than_its_share_of_the_whole() {
        // Code 8 closes only the 10k leg, which is 1 of 4 legs but carries
        // over half the network's conductance and 89 % of its gain.
        let share = volume_gain(8) / volume_gain(15);
        assert!(
            share > 0.85,
            "one leg of four should be most of it: {share}"
        );
    }

    /// C1's corner follows the closed legs. The ends of that range are what
    /// makes a note get darker as it decays.
    #[test]
    fn the_filter_corner_moves_with_the_volume_code() {
        let corner = |code: u8| {
            let g = volume_conductance(code) + 1.0 / BUS_LOAD;
            g / (2.0 * std::f64::consts::PI * C1)
        };
        assert!(
            (corner(1) - 673.0).abs() < 5.0,
            "code 1 corner {:.0}",
            corner(1)
        );
        assert!(
            (corner(15) - 3326.0).abs() < 10.0,
            "code 15 corner {:.0}",
            corner(15)
        );
    }

    /// Silence in, silence out, and no panic from a zero conductance: with
    /// every section open the cap still sees the bus load.
    #[test]
    fn silence_produces_silence() {
        let mut out = PacmanAudioOutput::new();
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
        let mut out = PacmanAudioOutput::new();
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
        let loud = sample_level(15) * volume_gain(15);
        let quiet = sample_level(15) * volume_gain(1);
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
        let mut out = PacmanAudioOutput::new();
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

    /// Full scale lands where the integer path's did, so an ear judgment of
    /// this stage is about its decay and its color and not about its level.
    #[test]
    fn full_scale_matches_the_path_this_replaces() {
        let peak = full_swing() * output_gain();
        assert!(
            (peak - LEGACY_FULL_SCALE).abs() < 1e-9,
            "full swing should render at {LEGACY_FULL_SCALE} of the rail, got {peak}"
        );
        // `OutputGain` clamps to +/-1, so a fraction at or above 1.0 is a stage
        // that saturates before a game has played anything.
        assert!(peak < 0.9, "full swing must leave headroom: {peak}");
    }
}
