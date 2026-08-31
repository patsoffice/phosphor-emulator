//! Mario Bros. (TMA1) discrete sound, built on the [`DiscreteCircuit`]
//! framework.
//!
//! Three discrete voices plus the music, all meeting at one summing node. The
//! transcription this is built from, with the netlists and the net table, is
//! `docs/schematics/mario-sound-sources.md`; what follows is the summary a
//! reader of this file needs and not the substance.
//!
//! | voice | trigger | source |
//! |---|---|---|
//! | Mario walk | a write to 0x7C00 | 74LS629 3.9 nF XOR 74LS629 22 nF, gated |
//! | Luigi walk | a write to 0x7C80 | 74LS629 39 nF XOR 74LS629 6.8 nF, gated |
//! | skid | 0x7F07, a level | a 4020 tap XOR a 74LS629, gated |
//! | music | the M58715's DAC | two LM3900 Norton sections |
//!
//! THE WALK TRIGGERS ARE WRITE STROBES, NOT LATCHES, and this is the thing to
//! get right about this board. The drawing labels those two inputs `7C00H(WR)`
//! and `7C80H(WR)`: the address decode ANDed with the write strobe, straight
//! into a 74123's trigger. The data byte does not exist as far as the circuit is
//! concerned - writing zero fires a footstep exactly as writing one does. Only
//! the skid is a level, and it is inverted.
//!
//! THE WALK VOICES ARE PERCUSSIVE, WHICH THE COMPONENT VALUES DO NOT SUGGEST
//! until they are worked through. Both oscillators in a walk voice run
//! ultrasonic - Mario's sweep 19 kHz to 39 kHz and 3.4 kHz to 7 kHz while its
//! one-shot is open - so their exclusive OR carries almost nothing a speaker
//! can reproduce. What survives the summing node's 1059 Hz corner is the
//! ENVELOPE: a 32 ms burst gated on and off. Measured on the board, 99.5 % of a
//! footstep's energy is below 150 Hz with a crest factor of 17. It is a click,
//! not a tone, and a model that got the oscillator frequencies wrong by a factor
//! of two would still sound about right, while one that got the gate wrong would
//! not sound like anything.
//!
//! THE LM3900 CHAIN IS THE MUSIC PATH, not the walks. The two Norton sections at
//! 3M take the sound CPU's DAC and nothing else; the three discrete voices reach
//! the summing node directly through their own resistors. Worth stating because
//! it is easy to read the sheet the other way round, and because it means this
//! device also owns the music filtering that `mario_bros.rs` previously
//! approximated with a bare coupling capacitor.

use phosphor_core::core::debug::{DebugRegister, Debuggable};

use phosphor_core::device::{
    DiscreteCircuit, DiscreteCircuitBuilder, ExternalSourceId, FilterMode, LogicInputId, LogicOp,
    Ls123Charge, Ls629, NodeId, OutputGain, PulseInputId,
};
use phosphor_macros::Saveable;

/// Output sample rate.
fn sample_rate() -> u64 {
    phosphor_core::audio::host_sample_rate() as u64
}

/// Internal simulation rate.
///
/// NOT high enough to render this board's oscillators cleanly, and that is a
/// deliberate, measured trade rather than an oversight. The walk oscillators
/// reach 39 kHz and 75 kHz, so representing their waveforms would need something
/// near a megahertz, which this framework cannot afford for one machine's sound.
///
/// What makes it acceptable here is where the voice's energy is. Everything
/// those oscillators produce passes the summing node's 1059 Hz corner, which is
/// 25 to 32 dB down across their range, and the audible output is the gate's
/// envelope rather than the tone. A square whose edges are quantised to this
/// rate still has the right mean and the right envelope; what it gains is alias
/// products that the same corner attenuates. The comparison against the board is
/// what justifies the choice, and it is recorded in the issue.
const SIM_RATE: u64 = 192_000;

/// Supply rail.
const VCC: f64 = 5.0;

/// The 74LS629's own frequency-control pin impedance. Needed here, unlike on the
/// Donkey Kong boards, because two of these pins share one node and the divider
/// is then against the pair rather than against one - see `WALK_FC_DIV`.
const LS629_PIN_R: f64 = 90_000.0;

// ---------------------------------------------------------------------------
// Logic levels
// ---------------------------------------------------------------------------
//
// A 7404 section's output, low and high. Not measured on this board: carried
// over from bench measurements of the same part on Donkey Kong Jr., a sibling
// Nintendo design of the same year. They matter because they are the endpoints
// of each oscillator's control sweep, so they set its frequency range.
const V_INV: (f64, f64) = (0.15, 4.14);
/// A 7408 section driving a mixer leg. Standard TTL levels; there is no pull-up
/// on this board's gates, unlike Donkey Kong Jr.'s.
///
/// These set the absolute loudness of all three voices equally, so they act as
/// one calibration rather than as a balance: the balance is the leg resistors'.
const V_AND: (f64, f64) = (0.2, 3.4);

// ---------------------------------------------------------------------------
// Component values, by designator, from TMA1-CPU sheet p39
// ---------------------------------------------------------------------------

/// Mario's one-shot timing resistor. The drawing reads 27 kΩ.
const R17: f64 = 27_000.0;
/// Luigi's. THE DRAWING READS 27 kΩ HERE TOO. An independent netlist of this
/// board uses 30 kΩ and notes "30K in schematics", which this scan does not
/// support; at 500 dpi both designators are unambiguous. Following the drawing
/// per the standing policy, and recording the disagreement because it is an 11 %
/// difference in Luigi's footstep length.
const R18: f64 = 27_000.0;
const R61: f64 = 47_000.0;

const R6: f64 = 4_700.0;
const R7: f64 = 4_700.0;
const R64: f64 = 20_000.0;
const R65: f64 = 10_000.0;

const R19: f64 = 22_000.0;
const R20: f64 = 22_000.0;
const R40: f64 = 22_000.0;
const R41: f64 = 100_000.0;
const R42: f64 = 43_000.0;
const R43: f64 = 100_000.0;

const R34: f64 = 2_000_000.0;
const R35: f64 = 1_000_000.0;
const R37: f64 = 750_000.0;
const R38: f64 = 360_000.0;
const R39: f64 = 750_000.0;

const C3: f64 = 10e-6;
const C4: f64 = 4.7e-6;
const C5: f64 = 0.039e-6;
const C6: f64 = 0.0039e-6;
const C14: f64 = 4.7e-6;
const C15: f64 = 4.7e-6;
const C16: f64 = 0.0068e-6;
const C17: f64 = 0.022e-6;
const C18: f64 = 100e-12;
const C20: f64 = 1e-6;
const C30: f64 = 100e-12;
const C31: f64 = 0.022e-6;
const C32: f64 = 1e-6;
const C39: f64 = 0.0047e-6;
const C40: f64 = 0.022e-6;
const C41: f64 = 4.7e-6;
const C43: f64 = 3.3e-6;
const C44: f64 = 3.3e-6;
const C47: f64 = 4.7e-6;

/// Every one-shot on this board charges its timing capacitor through a 1S953,
/// visible on the drawing beside each of D7, D8 and D10. That halves the pulse
/// against the datasheet's own configuration, so the footsteps are 32 ms and the
/// skid 55 ms rather than 57 ms and 99 ms. See [`Ls123Charge`].
const LS123_CHARGE: Ls123Charge = Ls123Charge::DiodeFed;

// ---------------------------------------------------------------------------
// Derived networks
// ---------------------------------------------------------------------------

/// TWO oscillator control pins share one node on each walk voice, fed through a
/// single resistor. The divider is therefore against the two pins in parallel,
/// 45 kΩ, not against one pin's 90 kΩ.
///
/// This is why the walk voices compute their control node here instead of
/// passing `r_freq` to [`Ls629`]: that argument divides against ONE pin, so
/// using it twice would apply the divider twice and put both oscillators at the
/// wrong pitch. The skid's two oscillators have a pin each and do use it. A
/// primitive's argument list is a topology, and an equivalent value is not
/// equivalent to it.
const WALK_PINS_R: f64 = LS629_PIN_R / 2.0;
const WALK1_FC_DIV: f64 = WALK_PINS_R / (R6 + WALK_PINS_R);
const WALK2_FC_DIV: f64 = WALK_PINS_R / (R7 + WALK_PINS_R);
/// Slew resistance at that node: the feed resistor in parallel with the pins.
const WALK1_FC_R: f64 = R6 * WALK_PINS_R / (R6 + WALK_PINS_R);
const WALK2_FC_R: f64 = R7 * WALK_PINS_R / (R7 + WALK_PINS_R);

/// The four mixer legs in parallel, 6832 Ω, which is what C31 works against:
/// a 1059 Hz low-pass on everything. Written as the reciprocal sum so changing
/// a leg moves the filter with it.
const MIX_R_PARALLEL: f64 = 1.0 / (1.0 / R20 + 1.0 / R19 + 1.0 / R40 + 1.0 / R41);
/// THERE ARE TWO OUTPUT COUPLINGS, NOT ONE, AND BOTH MATTER MORE THAN THEIR
/// CORNERS SUGGEST.
///
/// C32 into the follower's base bias network (R43 100 kΩ and R42 43 kΩ in
/// parallel, 30 kΩ) is 30 ms, and C47 into the 10 kΩ volume control at the
/// amplifier is 47 ms. Both sit near 5 Hz, so on a steady tone they do nothing
/// at all - which is exactly why it is tempting to fold them into one nominal
/// coupling and move on.
///
/// That is wrong here, because what this board sends them is not a tone. A
/// footstep is a rectangular pulse about 45 ms long, and a high-pass whose time
/// constant is comparable to a pulse's width decides whether that pulse arrives
/// as a pulse or as a pair of spikes. Modelled as one 10 ms coupling, the
/// board's 45 ms thump came out as a 10 ms tick: right at the onset, 11 dB down
/// by the middle of the event and gone by 100 ms where the board rings to 400.
///
/// The transistor loads the first of them, and by about as much again as the
/// bias network does, so leaving it out is not a rounding error. An emitter
/// follower's base sees `beta·(R62 + re)`: the divider puts the base at
/// `5·R42/(R42+R43)` = 1.50 V, so about 5.4 mA flows in the emitter, `re` is
/// 26 mV over that, and with a 2SC1815's typical beta the base looks like
/// roughly 31 kΩ. In parallel with the bias network that halves the coupling to
/// about 15 ms.
///
/// BETA IS THE ONE ESTIMATED QUANTITY IN THIS FILE. Everything else is a part on
/// the drawing; this is a datasheet typical for a transistor whose actual gain
/// the board never specifies, and the model is only weakly sensitive to it -
/// doubling beta moves this coupling by a third.
const FOLLOWER_BETA: f64 = 200.0;
const R62: f64 = 150.0;
/// Emitter current from the bias divider, and the resulting intrinsic emitter
/// resistance.
const FOLLOWER_IE: f64 = (VCC * R42 / (R42 + R43) - 0.7) / R62;
const FOLLOWER_RE: f64 = 0.026 / FOLLOWER_IE;
const FOLLOWER_ZIN: f64 = FOLLOWER_BETA * (R62 + FOLLOWER_RE);
const FOLLOWER_BIAS_R: f64 = R42 * R43 / (R42 + R43);
const FOLLOWER_BASE_R: f64 = 1.0 / (1.0 / FOLLOWER_BIAS_R + 1.0 / FOLLOWER_ZIN);
/// The volume control the amplifier's input coupling works into.
const VR1: f64 = 10_000.0;

/// The music path's coupling, C20 into R37 + R38. `1/(2π·1.11 MΩ·1 µF)` is
/// 0.143 Hz, far below anything audible: this capacitor strips the unipolar
/// ladder's pedestal rather than shaping the sound. Same value the machine used
/// before this device existed, and for the same reason.
const DAC_COUPLING_R: f64 = R37 + R38;

/// Stage 1 of the music filter: a Norton section with R34 in and R35 as
/// feedback, so a plain gain of R35/R34.
const DAC_STAGE1_GAIN: f64 = R35 / R34;

/// Stage 2 is a two-pole active low-pass, and its poles are both real, so it is
/// spelled as a second-order section whose Q puts them where the parts do.
///
/// Solving the network - C18 shunting the R37/R38 junction, R39 with C30 across
/// it as feedback - gives poles at `(1/R37 + 1/R38)/(2π·C18)` and
/// `1/(2π·R39·C30)`, which is 6543 Hz and 2122 Hz, and a DC gain of
/// `R39/(R37 + R38)`.
///
/// Written out of the parts rather than as those two numbers, so that changing
/// a resistor moves the filter with it instead of leaving a stale constant.
const TAU: f64 = std::f64::consts::TAU;
const DAC_POLE_A: f64 = (1.0 / R37 + 1.0 / R38) / (TAU * C18);
const DAC_POLE_B: f64 = 1.0 / (TAU * R39 * C30);
const DAC_STAGE2_GAIN: f64 = R39 / (R37 + R38);

/// The R2R ladder's full-scale output. The music DAC on this board is a discrete
/// resistor ladder off a 374 latch, not the DAC-08 the Donkey Kong boards use.
const DAC_FULL_SCALE_V: f64 = 3.4;

/// Output calibration: modelled volts at the amplifier's input to full scale.
/// The only scalar here that is not a component value, and it changes no balance
/// between voices - the leg resistors already set that.
const OUTPUT_GAIN: f64 = 0.6;

/// Handles the board writes reach.
#[derive(Clone, Copy)]
struct MarioInputs {
    dac: ExternalSourceId,
    /// Mario's and Luigi's walk lines are pulses because the hardware's are: the
    /// write strobe is the trigger and there is no level to hold.
    walk: [PulseInputId; 2],
    skid: LogicInputId,
    mix: NodeId,
}

/// One walk voice: a one-shot, a shared control node, two oscillators beating
/// against each other, and a gate.
///
/// Returned as the gated logic node; the caller renders it at TTL levels and
/// gives it its mixer leg.
#[allow(clippy::too_many_arguments)]
fn walk_voice(
    b: &mut DiscreteCircuitBuilder,
    name: &str,
    trigger: PulseInputId,
    r_shot: f64,
    c_shot: f64,
    fc_div: f64,
    fc_r: f64,
    c_slew: f64,
    c_osc_a: f64,
    c_osc_b: f64,
) -> NodeId {
    let shot = b.ls123(
        &format!("{name}_SHOT"),
        trigger,
        LS123_CHARGE,
        r_shot,
        c_shot,
    );
    // The 74123's Q-bar drives a 7404, so the inverter's output follows Q: the
    // control node rises while the footstep sounds and falls back between them.
    let drive = b.logic_levels(&format!("{name}_FC_DRIVE"), shot, V_INV.0, V_INV.1);
    let divided = b.gain(&format!("{name}_FC_DIV"), drive, fc_div);
    let fc = b.rc_low_pass(&format!("{name}_FC"), divided, fc_r, c_slew);

    let osc = |b: &mut DiscreteCircuitBuilder, suffix: &str, c: f64| {
        b.ls629_vco(
            &format!("{name}_OSC_{suffix}"),
            fc,
            Ls629 {
                c,
                // Already divided and slewed above, at the shared node.
                r_freq: 0.0,
                c_freq_in: 0.0,
                v_rng: VCC,
                r_rng: 0.0,
            },
        )
    };
    let osc_a = osc(b, "A", c_osc_a);
    let osc_b = osc(b, "B", c_osc_b);
    let sq_a = b.variable_square(&format!("{name}_SQ_A"), osc_a);
    let sq_b = b.variable_square(&format!("{name}_SQ_B"), osc_b);
    let beat = b.logic_gate(&format!("{name}_XOR"), LogicOp::Xor, sq_a, sq_b);
    b.logic_gate(&format!("{name}_AND"), LogicOp::And, shot, beat)
}

fn build_circuit() -> (DiscreteCircuit, MarioInputs) {
    let rate = sample_rate();
    let mut b = DiscreteCircuitBuilder::new(rate, rate).with_sim_rate(SIM_RATE);

    let dac = b.external_source("DAC");
    let walk1_trig = b.pulse_input("WALK1_STROBE");
    let walk2_trig = b.pulse_input("WALK2_STROBE");
    let skid_en = b.logic_input("SKID_EN");

    // -----------------------------------------------------------------------
    // The two walk voices
    // -----------------------------------------------------------------------
    let walk1 = walk_voice(
        &mut b,
        "WALK1",
        walk1_trig,
        R17,
        C14,
        WALK1_FC_DIV,
        WALK1_FC_R,
        C3,
        C6,
        C17,
    );
    let walk1_out = b.logic_levels("WALK1_OUT", walk1, V_AND.0, V_AND.1);
    let walk2 = walk_voice(
        &mut b,
        "WALK2",
        walk2_trig,
        R18,
        C15,
        WALK2_FC_DIV,
        WALK2_FC_R,
        C4,
        C5,
        C16,
    );
    let walk2_out = b.logic_levels("WALK2_OUT", walk2, V_AND.0, V_AND.1);

    // -----------------------------------------------------------------------
    // Skid
    // -----------------------------------------------------------------------
    //
    // Structurally this is Donkey Kong Jr.'s walking voice: one oscillator
    // clocks a 4020, a tap of that counter comes back through an inverter to
    // the SAME oscillator's control pin, and a second tap is exclusive-ORed
    // with the other oscillator. The loop is the voice.
    //
    // Unlike the two walks above, each of these oscillators has a control pin to
    // itself, so the divider and the slew capacitor go to the primitive.
    let skid_shot = b.ls123("SKID_SHOT", skid_en, LS123_CHARGE, R61, C41);
    // Here the 7404 is fed by Q rather than Q-bar, so this control node FALLS
    // while the skid sounds - the opposite of the walks, on the same board.
    let skid_fc_a = b.logic_levels("SKID_FC_A_DRIVE", skid_shot, V_INV.1, V_INV.0);
    let skid_osc_a = b.ls629_vco(
        "SKID_OSC_A",
        skid_fc_a,
        Ls629 {
            c: C40,
            r_freq: R65,
            c_freq_in: C44,
            v_rng: VCC,
            r_rng: 0.0,
        },
    );
    let skid_sq_a = b.variable_square("SKID_SQ_A", skid_osc_a);

    let skid_fc_b = b.feedback_node("SKID_FC_B_RING");
    let skid_osc_b = b.ls629_vco(
        "SKID_OSC_B",
        skid_fc_b,
        Ls629 {
            c: C39,
            r_freq: R64,
            c_freq_in: C43,
            v_rng: VCC,
            r_rng: 0.0,
        },
    );
    let counter = b.ripple_counter("COUNTER_3H", skid_osc_b, 14);
    // The 4020's outputs are numbered from one, so Q4 and Q12 are stages 4 and
    // 12, which are bits 3 and 11 of the count.
    let q4 = b.bit_decode("COUNTER_Q4", counter, 3);
    let q12 = b.bit_decode("COUNTER_Q12", counter, 11);
    let skid_fc_b_node = b.logic_levels("SKID_FC_B_DRIVE", q12, V_INV.1, V_INV.0);
    b.connect(skid_fc_b, skid_fc_b_node);

    let skid_beat = b.logic_gate("SKID_XOR", LogicOp::Xor, q4, skid_sq_a);
    let skid_gated = b.logic_gate("SKID_AND", LogicOp::And, skid_shot, skid_beat);
    let skid_out = b.logic_levels("SKID_OUT", skid_gated, V_AND.0, V_AND.1);

    // -----------------------------------------------------------------------
    // Music: the DAC through two LM3900 Norton sections
    // -----------------------------------------------------------------------
    let dac_in = b.gain("DAC_STAGE1", dac, DAC_STAGE1_GAIN);
    let dac_ac = b.rc_high_pass("DAC_AC", dac_in, DAC_COUPLING_R, C20);
    // Both poles are real, so f0 is their geometric mean and Q is set to place
    // them exactly rather than chosen for a shape.
    let f0 = (DAC_POLE_A * DAC_POLE_B).sqrt();
    let q = f0 / (DAC_POLE_A + DAC_POLE_B);
    let dac_lp = b.second_order("DAC_LP", dac_ac, FilterMode::LowPass, f0, q);
    let dac_out = b.gain("DAC_OUT", dac_lp, DAC_STAGE2_GAIN);

    // -----------------------------------------------------------------------
    // Mixer and output
    // -----------------------------------------------------------------------
    //
    // Four legs and a shunt capacitor. The skid's 100 kΩ against the others'
    // 22 kΩ makes it much the quietest, which is the board's judgement.
    let mix = b.resistor_mixer(
        "MIX",
        &[
            (walk1_out, R20),
            (walk2_out, R19),
            (dac_out, R40),
            (skid_out, R41),
        ],
        None,
    );
    // C31 across the summing node. This is most of what makes the walk voices
    // audible as thumps rather than as ultrasonic hash.
    let mix_lp = b.rc_low_pass("MIX_LP", mix, MIX_R_PARALLEL, C31);
    // C32 into the emitter follower's base. Every voice idles at a gate's low
    // level and the DAC is unipolar, so this is what removes the pedestal.
    let base = b.rc_high_pass("FOLLOWER_BASE", mix_lp, FOLLOWER_BASE_R, C32);
    // Q10 buffers, and C47 couples its emitter into the volume control at the
    // amplifier. The follower itself is unity and its emitter network (R62 with
    // C42, 10.6 kHz) sits above everything this board puts through it, so it is
    // named here rather than modelled.
    let out = b.rc_high_pass("AMP_IN", base, VR1, C47);
    b.output(out, OutputGain::linear(OUTPUT_GAIN));

    let circuit = b.build();
    (
        circuit,
        MarioInputs {
            dac,
            walk: [walk1_trig, walk2_trig],
            skid: skid_en,
            mix,
        },
    )
}

/// Mario Bros. discrete sound: two footstep voices, the skid, and the M58715's
/// DAC, mixed the way the board mixes them.
#[derive(Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct MarioDiscreteSound {
    #[save(id = 1)]
    circuit: DiscreteCircuit,
    #[save_skip]
    ids: MarioInputs,
    #[save(id = 2)]
    skid: bool,
}

impl MarioDiscreteSound {
    pub fn new() -> Self {
        let (circuit, ids) = build_circuit();
        Self {
            circuit,
            ids,
            skid: false,
        }
    }

    /// A write to 0x7C00 (`player` 0) or 0x7C80 (`player` 1) fires that
    /// player's footstep.
    ///
    /// There is no value, because the hardware has none: the board ANDs the
    /// address decode with the write strobe and feeds that straight to a 74123.
    /// Writing zero is a footstep.
    pub fn strobe_walk(&mut self, player: usize) {
        if let Some(&id) = self.ids.walk.get(player) {
            self.circuit.pulse(id);
        }
    }

    /// The skid line, 0x7F07 bit 0. A level rather than a strobe, and inverted
    /// on the board, which is folded in here so the caller passes the bit as the
    /// game wrote it.
    pub fn set_skid(&mut self, on: bool) {
        self.skid = on;
        self.circuit.set_logic(self.ids.skid, on);
    }

    /// Feed one box-filtered DAC sample and advance the circuit by one output
    /// sample's worth of simulation.
    pub fn feed_dac(&mut self, sample: i16) {
        self.circuit
            .set_external(self.ids.dac, sample as f64 / 32767.0 * DAC_FULL_SCALE_V);
        self.circuit.tick(1);
    }

    pub fn fill_audio(&mut self, out: &mut [i16]) -> usize {
        self.circuit.fill_audio(out)
    }

    /// The built circuit, for tooling that reads individual stages.
    pub fn circuit(&self) -> &DiscreteCircuit {
        &self.circuit
    }

    pub fn reset(&mut self) {
        self.circuit.reset();
        self.skid = false;
    }
}

impl Default for MarioDiscreteSound {
    fn default() -> Self {
        Self::new()
    }
}

impl phosphor_core::device::Device for MarioDiscreteSound {
    fn name(&self) -> &'static str {
        "Mario Discrete"
    }
    fn reset(&mut self) {
        self.reset();
    }
}

impl Debuggable for MarioDiscreteSound {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        let sample = |v: f64| (v.clamp(-1.0, 1.0) * 32767.0) as i16 as u16 as u64;
        vec![
            DebugRegister {
                name: "SKID",
                value: self.skid as u64,
                width: 8,
            },
            DebugRegister {
                name: "MIX_OUT",
                value: sample(self.circuit.value(self.ids.mix)),
                width: 16,
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::save_state::{Saveable as _, StateReader, StateWriter};

    fn run(s: &mut MarioDiscreteSound, n: usize) {
        for _ in 0..n {
            s.feed_dac(0);
        }
    }

    fn drain_rms(s: &mut MarioDiscreteSound) -> f64 {
        let mut buf = vec![0i16; 1 << 18];
        let n = s.fill_audio(&mut buf);
        if n == 0 {
            return 0.0;
        }
        let sum: f64 = buf[..n].iter().map(|&v| (v as f64).powi(2)).sum();
        (sum / n as f64).sqrt()
    }

    /// Settle the analog state and discard it. Every voice idles at a gate's low
    /// level and the DAC is unipolar, so power-on puts a step through two
    /// coupling capacitors whose time constants are 15 ms and 47 ms.
    fn settle(s: &mut MarioDiscreteSound) {
        run(s, 88_200);
        let mut discard = vec![0i16; 1 << 18];
        while s.fill_audio(&mut discard) > 0 {}
    }

    /// An untriggered board is silent.
    ///
    /// Worth asserting because nothing on this board ever stops: five 74LS629
    /// halves and a 4020 free-run for as long as it is powered, with no enable
    /// anywhere. What keeps them inaudible is the AND gate downstream of each,
    /// and a gate wired the wrong way round leaks its voice into every second of
    /// play rather than failing outright.
    #[test]
    fn an_untriggered_board_is_silent() {
        let mut s = MarioDiscreteSound::new();
        settle(&mut s);
        run(&mut s, 44_100);
        let mut buf = vec![0i16; 1 << 18];
        let n = s.fill_audio(&mut buf);
        assert!(n > 40_000, "expected about a second of audio, got {n}");
        let peak = buf[..n].iter().map(|v| v.abs()).max().unwrap();
        assert_eq!(peak, 0, "an idle board put out a peak of {peak}");
    }

    #[test]
    fn each_trigger_drives_its_own_voice() {
        let mut walk = [0.0f64; 2];
        for (player, out) in walk.iter_mut().enumerate() {
            let mut s = MarioDiscreteSound::new();
            settle(&mut s);
            s.strobe_walk(player);
            run(&mut s, 44_100);
            *out = drain_rms(&mut s);
        }
        let mut s = MarioDiscreteSound::new();
        settle(&mut s);
        s.set_skid(true);
        run(&mut s, 44_100 * 3 / 60);
        s.set_skid(false);
        run(&mut s, 44_100);
        let skid = drain_rms(&mut s);

        for (player, rms) in walk.iter().enumerate() {
            assert!(*rms > 100.0, "walk {player} produced only {rms:.1} RMS");
        }
        assert!(skid > 20.0, "the skid produced only {skid:.1} RMS");
        // The skid's mixer leg is 100 kΩ against a footstep's 22 kΩ, so the
        // board makes it much the quietest of the three. Asserted as an ordering
        // rather than a ratio because it is the leg resistors that set it and
        // nothing here should be free to change it.
        assert!(
            skid < walk[0] && skid < walk[1],
            "the skid ({skid:.1}) should be quieter than either footstep \
             ({:.1}, {:.1})",
            walk[0],
            walk[1]
        );
    }

    /// The two footsteps are DIFFERENT sounds, not one voice on two lines.
    ///
    /// They differ only in their oscillator capacitors - 3.9 nF and 22 nF for
    /// Mario, 39 nF and 6.8 nF for Luigi - so a model that wired both lines to
    /// one voice, or that swapped a capacitor pair, would pass every other check
    /// in this file.
    #[test]
    fn the_two_footsteps_are_different_sounds() {
        let render = |player: usize| -> Vec<i16> {
            let mut s = MarioDiscreteSound::new();
            settle(&mut s);
            s.strobe_walk(player);
            run(&mut s, 22_050);
            let mut buf = vec![0i16; 1 << 18];
            let n = s.fill_audio(&mut buf);
            buf.truncate(n);
            buf
        };
        let a = render(0);
        let b = render(1);
        assert_eq!(a.len(), b.len());
        assert!(a != b, "both footsteps rendered identically");
    }

    #[test]
    fn save_load_round_trips_mid_effect() {
        let mut a = MarioDiscreteSound::new();
        a.strobe_walk(0);
        a.set_skid(true);
        run(&mut a, 5_000);

        let mut w = StateWriter::new();
        a.save_state(&mut w);
        let data = w.into_vec();

        let mut b = MarioDiscreteSound::new();
        let mut r = StateReader::new(&data);
        b.load_state(&mut r).unwrap();

        let mut discard = vec![0i16; 1 << 18];
        while a.fill_audio(&mut discard) > 0 {}
        while b.fill_audio(&mut discard) > 0 {}

        run(&mut a, 4_000);
        run(&mut b, 4_000);
        let mut sa = vec![0i16; 1 << 18];
        let mut sb = vec![0i16; 1 << 18];
        let na = a.fill_audio(&mut sa);
        let nb = b.fill_audio(&mut sb);
        assert_eq!(na, nb);
        assert_eq!(sa[..na], sb[..nb]);
    }
}
