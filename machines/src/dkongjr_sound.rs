//! Donkey Kong Jr. discrete sound, built on the [`DiscreteCircuit`] framework.
//!
//! Donkey Kong Jr. shares the TKG-04 board with Donkey Kong and does not share
//! its sound. The two boards have the DAC-08 at 8K, the MB8884 at 7H and the
//! MB3712 output stage in common, and nothing else in the audio path: Donkey
//! Kong's effects come from two voltage-controlled 555 astables and an
//! LS164-into-LS161 chain, and not one of those parts is on this drawing. What
//! is here instead is five 74LS629 oscillator halves across 5K, 8L and 7P, a
//! 4020 ripple counter, an LS157 selecting its taps, three LS123 one-shots and
//! a 16-bit LFSR across two LS164s. The transcription this is built from is
//! `docs/schematics/dkongjr-sound-sources.md`.
//!
//! FOUR VOICES, not the three Donkey Kong has, and a fifth latch bit that
//! retunes one of them:
//!
//! | voice | trigger | source |
//! |---|---|---|
//! | walking | 6H bit 0 | a counter tap exclusive-ORed with a gated tone |
//! | jump | 6H bit 1 | an oscillator swept by a one-shot, chopped by Q3 |
//! | climbing | 6H bit 2 | LFSR noise chopped by Q2 |
//! | falling | 5H bit 1 | an oscillator gated by its own enable |
//! | (walking pitch) | 6H bit 7 | picks which pair of counter taps walking uses |
//!
//! Two of those five bits reached no sound device before this existed, and the
//! other three reached a model of the wrong board.
//!
//! THE GAME'S OWN TRIGGER DISCIPLINE was measured rather than assumed, because
//! the drawing settles what the parts are and not how they are driven. Over
//! 3000 frames of recorded play (`tools/script/examples/dkongjr_sound_trace.rhai`)
//! the three one-shot voices are each asserted for exactly three frames, about
//! 50 ms, and falling is *held* for 86 frames. That is the transcription
//! confirming itself against the game: the voices the drawing calls one-shots
//! get edges, and the one voice it calls an enable gets a level. It also means
//! the one-shot widths below are the lengths of the sounds, since the game's
//! pulse is far shorter than any of them.
//!
//! THE DAC PATH IS DONKEY KONG'S, part for part: the same Q7 signal-decay
//! network of 10 kΩ across 10 µF and the same Sallen-Key reconstruction filter
//! at 1916 Hz with Q 0.74. That half of [`crate::dkong_sound`] transfers
//! unchanged and is not re-derived here.

use phosphor_core::core::debug::{DebugRegister, Debuggable};

use phosphor_core::device::{
    DiscreteCircuit, DiscreteCircuitBuilder, ExternalSourceId, FilterMode, LfsrOutput, LfsrShift,
    LfsrSpec, LogicInputId, LogicOp, Ls123Charge, Ls629, NodeId, OutputGain,
};
use phosphor_macros::Saveable;

/// Output sample rate.
fn sample_rate() -> u64 {
    phosphor_core::audio::host_sample_rate() as u64
}

/// Internal simulation rate. The fastest node needing a *waveform* is the 5K
/// pin 7 tone at up to 14 kHz, which this carries with room to spare. The
/// board's fastest oscillator is four times that, but it exists only to clock
/// the 4020 and is modelled as a rate rather than a square precisely so that it
/// does not set this number.
const SIM_RATE: u64 = 192_000;

/// Supply rail.
const VCC: f64 = 5.0;

// ---------------------------------------------------------------------------
// Measured logic levels
// ---------------------------------------------------------------------------
//
// What a 74LS04 section on this board actually drives, low and high, measured
// per gate. These are not ours; they come from the same bench work as the
// oscillator calibration in `phosphor_core`'s `ls629_frequency`.
//
// They are kept per gate rather than averaged into one pair because each one is
// the endpoint of its own oscillator's measured sweep: 0.151 V on 5K pin 7's
// driver is the exact voltage at which that oscillator was measured at 3139 Hz.
// Rounding them together would move each oscillator off the points the model is
// calibrated against, which is the opposite of tidying up.
/// 4F pin 10, driving 5K pin 7's control node.
const V_4F: (f64, f64) = (0.151, 4.14);
/// 5J pin 8, driving 5K pin 10's control node.
const V_5J: (f64, f64) = (0.135, 4.15);
/// 7N pins 4 and 6, driving 8L pin 10's control node.
const V_7N: (f64, f64) = (0.151, 4.14);
/// 7N pin 8, driving 7P pin 7's control node.
const V_7N_FALL: (f64, f64) = (0.134, 4.16);
/// A 74LS00 section at 5N with a 1 kΩ pull-up, which is what the walking and
/// falling voices drive the mixer through. The high level is above a bare TTL
/// output's because of that pull-up.
const V_5N: (f64, f64) = (0.2, 4.9);
/// A 74LS629's own output when driving a transistor base, measured. Lower than
/// the pulled-up NAND above, which is the point of measuring both.
const V_LS629_OUT: f64 = 4.5;

/// Invert a 0/1 logic node.
///
/// This is a MODEL artifact, not a gate: `rc_disc_modulated` takes a trigger
/// that is active low, because the board it was first written for reaches that
/// stage through an inverter and this one does not. Nothing on the Donkey Kong
/// Jr. drawing corresponds to it, so it is spelled out here rather than dressed
/// up as a 74LS04 section that is not there.
fn invert_logic(b: &mut DiscreteCircuitBuilder, name: &str, src: NodeId) -> NodeId {
    let one = b.constant(&format!("{name}_ONE"), 1.0);
    let neg = b.gain(&format!("{name}_NEG"), src, -1.0);
    b.add(name, &[one, neg])
}

// ---------------------------------------------------------------------------
// Component values, by designator, from sheet 5 of the Donkey Kong Junior CPU
// P.C. Board drawing
// ---------------------------------------------------------------------------

const R2: f64 = 120.0;
const R3: f64 = 100_000.0;
const R4: f64 = 47_000.0;
const R5: f64 = 150_000.0;
const R6: f64 = 20_000.0;
const R8: f64 = 47_000.0;
const R9: f64 = 47_000.0;
const R10: f64 = 10_000.0;
const R11: f64 = 20_000.0;
const R12: f64 = 10_000.0;
const R13: f64 = 47_000.0;
const R14: f64 = 30_000.0;
const R17: f64 = 47_000.0;
const R18: f64 = 100_000.0;
const R19: f64 = 100.0;
const R20: f64 = 10_000.0;
const R24: f64 = 4_700.0;
const R25: f64 = 47_000.0;
const R27: f64 = 10_000.0;
const R28: f64 = 100_000.0;
const R33: f64 = 1_000.0;
const R34: f64 = 1_000.0;
const R35: f64 = 1_000.0;

const C13: f64 = 4.7e-6;
const C14: f64 = 4.7e-6;
const C15: f64 = 22e-6;
const C16: f64 = 3.3e-6;
const C17: f64 = 3.3e-6;
const C18: f64 = 22e-9;
const C19: f64 = 4.7e-9;
const C20: f64 = 0.12e-6;
const C21: f64 = 56e-9;
const C22: f64 = 220e-9;
const C23: f64 = 0.47e-6;
const C24: f64 = 47e-6;
const C25: f64 = 1e-6;
const C26: f64 = 47e-6;
const C27: f64 = 22e-6;
const C28: f64 = 10e-6;
const C29: f64 = 10e-6;
const C30: f64 = 0.47e-6;
const C32: f64 = 10e-6;
const C37: f64 = 0.12e-6;

/// How this board charges its one-shot timing capacitors, which sets their pulse
/// widths: 56 ms for walking, 264 ms for jump and climbing.
///
/// THIS WAS WRONG FIRST TIME and the mistake is worth keeping written down,
/// because the reasoning that produced it was good. The 74LS123 datasheet gives
/// `tW = 0.45·Rext·Cext`, states both of its conditions in a way these parts
/// satisfy comfortably, and adds that the switching diode "is not needed for
/// electrolytic capacitance application and should not be used on the LS122 and
/// LS123" — and C14, C15 and C27 are electrolytics. Taking the datasheet at its
/// word gave a 99 ms footstep.
///
/// The board is not the datasheet's circuit. Mario Bros., the sibling Nintendo
/// design, fits the diode on all three of its one-shots, with electrolytic
/// timing capacitors and even the same C14 designator and value. Against the
/// reference this board's footstep attack measures 57 ms, which is 0.25·R·C to
/// within 2 % and nothing like 0.45·R·C.
///
/// The general form is the one the design doc already carries about two 555s on
/// one sheet: the same components in the same count can be two circuits, and
/// which one you have is a question about the drawing rather than about the
/// values.
const LS123_CHARGE: Ls123Charge = Ls123Charge::DiodeFed;

/// The mixer's shunt capacitor at the summing node. See the comment where it is
/// used: this is the one value here that does not come from the drawing.
const C_MIX: f64 = 0.01e-6;
/// The five mixer legs in parallel, which is the resistance [`C_MIX`] works
/// against: 9156 Ω, so the corner is 1738 Hz. Written as the reciprocal sum
/// rather than as that number, so changing a leg moves the filter with it.
const MIX_R_PARALLEL: f64 = 1.0 / (1.0 / R5 + 1.0 / R3 + 1.0 / R6 + 1.0 / R4 + 1.0 / R25);

/// The amplifier's input coupling: C13 against the MB3712's 1 kΩ input, 34 Hz.
/// This is what removes the DC every voice rests at, since each one idles at a
/// gate's output level rather than at zero.
const AMP_R: f64 = 1_000.0;

/// The DAC's signal-decay network, Q7 with R20 across C32: a sample fades with
/// τ = 100 ms once the sound CPU drops its decay line rather than ending on a
/// step. Identical to Donkey Kong's.
const DAC_DECAY_S: f64 = R20 * C32;
/// I8035 DAC reconstruction filter, a Sallen-Key low-pass with 5.6 kΩ ×2 and
/// 22 nF / 10 nF: f ≈ 1916 Hz, Q ≈ 0.74. Identical to Donkey Kong's.
const DAC_LP_HZ: f64 = 1_916.0;
const DAC_LP_Q: f64 = 0.74;

/// Normalized DAC stream to volts. The I8035 drives a DAC-08 against the 5 V
/// rail, so a full-scale code is the rail.
const DAC_V: f64 = VCC;

/// Output calibration: modelled volts at the amplifier's input to full scale.
/// The only scalar here that is not a component value, and it is a calibration
/// rather than a fitted parameter — it converts the circuit's volts into the
/// finite range PCM has, and changes no balance between voices, which the
/// mixer's leg resistors already set.
const OUTPUT_GAIN: f64 = 0.5;

/// The two 4020 taps each LS157 channel can present, and which latch state
/// picks which. Stage `n` of the counter divides its clock by `2^(n+1)`, so
/// these are divide-by-16 and divide-by-128 on one channel and by 4096 and 8192
/// on the other.
const WALK_TAP_LOW: (u8, u8) = (3, 6);
const WALK_TAP_HIGH: (u8, u8) = (11, 12);

/// The 4020's stage count. Only four stages are read, but the counter is a
/// 14-stage part and its wrap is what makes the read taps periodic.
const COUNTER_STAGES: u8 = 14;

/// The climbing noise register: two LS164s cascaded into 16 bits, with QC of
/// the first (bit 2) exclusive-ORed with QH of the second (bit 15), that XOR
/// inverted through 7N and shifted back into bit 0.
///
/// The output is the XOR itself, not the register, which is what the drawing
/// shows and what makes the tap positions the whole character of the noise.
/// Power-on state is zero, which a real register does and which is only a valid
/// seed *because* the feedback is inverted: an uninverted XOR of two zero taps
/// would shift in zeroes for ever.
fn climb_lfsr() -> LfsrSpec {
    LfsrSpec {
        width: 16,
        taps: (2, 15),
        seed: 0,
        shift: LfsrShift::TowardHigh,
        invert_feedback: true,
        output: LfsrOutput::Feedback,
    }
}

/// Handles the board writes reach.
#[derive(Clone, Copy)]
struct DkongJrInputs {
    dac: ExternalSourceId,
    walk_en: LogicInputId,
    jump_en: LogicInputId,
    climb_en: LogicInputId,
    /// 6H bit 7, inverted by 5J pin 12 on its way to the LS157's select input.
    walk_pitch: LogicInputId,
    fall_en: LogicInputId,
    discharge: LogicInputId,
    mix: NodeId,
}

fn build_circuit() -> (DiscreteCircuit, DkongJrInputs) {
    let rate = sample_rate();
    let mut b = DiscreteCircuitBuilder::new(rate, rate).with_sim_rate(SIM_RATE);

    let dac = b.external_source("DAC");
    let walk_en = b.logic_input("WALK_EN");
    let jump_en = b.logic_input("JUMP_EN");
    let climb_en = b.logic_input("CLIMB_EN");
    let fall_en = b.logic_input("FALL_EN");
    let discharge = b.logic_input("DISCHARGE");
    // 6H bit 7 reaches the LS157's select pin through the inverter at 5J pin 12,
    // so the latch bit and the selected channel are opposite. Declaring the
    // input inverted puts that gate where it belongs instead of folding it into
    // the channel order, where it would be invisible.
    let walk_pitch = b.inverted_logic_input("WALK_PITCH_SEL");

    let gnd = b.constant("GND", 0.0);

    // -----------------------------------------------------------------------
    // Walking (6H bit 0, retuned by 6H bit 7)
    // -----------------------------------------------------------------------
    //
    // The voice is a loop, which is why it earns a netlist in the transcription
    // and why a block diagram cannot show it: the 4020 is clocked by 5K pin 10,
    // one of its taps is selected by the LS157, and the selected tap goes back
    // through an inverter to 5K pin 10's own frequency control. The oscillator
    // that clocks the counter is tuned by the counter it clocks. The framework
    // cuts that loop with a one-step delay at 192 kHz, which is far below
    // anything audible here.
    //
    // What comes out is not a tone under an envelope. It is the exclusive OR of
    // a counter tap with a gated tone, gated again by the one-shot: a footstep
    // is a burst of a two-source beat, not a note.
    let walk_shot = b.ls123("WALK_SHOT", walk_en, LS123_CHARGE, R8, C14);
    // 4F inverts the one-shot, so an asserted footstep pulls this control node
    // DOWN and the tone falls from about 14 kHz to about 3 kHz over the ~30 ms
    // C17 needs. The pitch is moving for most of the footstep's length.
    let fc_5k_a = b.logic_levels("FC_5K_A", walk_shot, V_4F.1, V_4F.0);
    let vco_5k_a = b.ls629_vco(
        "VCO_5K_A",
        fc_5k_a,
        Ls629 {
            c: C18,
            r_freq: R10,
            c_freq_in: C17,
            v_rng: VCC,
            r_rng: R33,
        },
    );
    let tone_5k_a = b.variable_square("TONE_5K_A", vco_5k_a);

    // The counter's clock. Its control node is the selected high tap inverted,
    // and C16 slews it, so this oscillator wanders between about 14 kHz and
    // 59 kHz at the rate of the tap driving it. Reserved before the ring is
    // built so the cut point in the loop is chosen here rather than falling out
    // of node order.
    let fc_5k_b = b.feedback_node("FC_5K_B_RING");
    let vco_5k_b = b.ls629_vco(
        "VCO_5K_B",
        fc_5k_b,
        Ls629 {
            c: C19,
            r_freq: R11,
            c_freq_in: C16,
            v_rng: VCC,
            r_rng: R33,
        },
    );
    let counter = b.ripple_counter("COUNTER_6L", vco_5k_b, COUNTER_STAGES);
    let tap_lo_a = b.bit_decode("TAP_3", counter, WALK_TAP_LOW.0);
    let tap_lo_b = b.bit_decode("TAP_6", counter, WALK_TAP_LOW.1);
    let tap_hi_a = b.bit_decode("TAP_11", counter, WALK_TAP_HIGH.0);
    let tap_hi_b = b.bit_decode("TAP_12", counter, WALK_TAP_HIGH.1);

    // The LS157 at 6K, all three channels. `walk_pitch` is already the inverted
    // latch bit, so it IS the select pin: asserted picks the faster pair of taps
    // and lets the tone through, released picks the slower pair and grounds the
    // tone's channel. One latch bit therefore changes both the beat frequency
    // and whether there is a second source to beat against.
    let mux_tone = b.select("MUX_6K_4", walk_pitch, gnd, tone_5k_a);
    let mux_low = b.select("MUX_6K_7", walk_pitch, tap_lo_b, tap_lo_a);
    let mux_high = b.select("MUX_6K_9", walk_pitch, tap_hi_b, tap_hi_a);

    // The back edge: 5J pin 8 inverts the selected high tap into 5K pin 10's
    // control, closing the ring reserved above.
    let fc_5k_b_node = b.logic_levels("FC_5K_B", mux_high, V_5J.1, V_5J.0);
    b.connect(fc_5k_b, fc_5k_b_node);

    let walk_xor = b.logic_gate("XOR_6N", LogicOp::Xor, mux_tone, mux_low);
    let walk_nand = b.logic_gate("NAND_5N_11", LogicOp::Nand, walk_shot, walk_xor);
    let walk = b.logic_levels("WALK_OUT", walk_nand, V_5N.0, V_5N.1);

    // -----------------------------------------------------------------------
    // Jump (6H bit 1)
    // -----------------------------------------------------------------------
    //
    // Jump is NOT independent of walking, and this is the place the first pass
    // of the transcription went wrong. Its oscillator's control node has two
    // sources through different resistors: the one-shot through R13, and the
    // walking counter's bit 11 through R12. So a jump taken while Junior is
    // walking is a different sound from one taken standing still.
    let jump_shot = b.ls123("JUMP_SHOT", jump_en, LS123_CHARGE, R9, C15);
    let jump_leg_shot = b.logic_levels("JUMP_LEG_SHOT", jump_shot, V_7N.0, V_7N.1);
    let jump_leg_tap = b.logic_levels("JUMP_LEG_TAP", tap_hi_a, V_7N.1, V_7N.0);
    // Two legs into one node is a resistor mixer, and what the oscillator's pin
    // sees is that network's Thevenin equivalent: this node is the open-circuit
    // voltage, and R13 in parallel with R12 is the source resistance the pin
    // divides against. Passing the parallel value as `r_freq` is right for that
    // reason and not because it is "the equivalent resistor" — the two legs are
    // still separate arguments here, where the voltage is formed.
    let fc_8l_raw = b.resistor_mixer(
        "FC_8L_RAW",
        &[(jump_leg_shot, R13), (jump_leg_tap, R12)],
        None,
    );
    let vco_8l = b.ls629_vco(
        "VCO_8L",
        fc_8l_raw,
        Ls629 {
            c: C22,
            r_freq: R13 * R12 / (R13 + R12),
            c_freq_in: C24,
            v_rng: VCC,
            r_rng: R35,
        },
    );
    let tone_8l = b.variable_square("TONE_8L", vco_8l);
    let tone_8l_v = b.logic_levels("TONE_8L_V", tone_8l, 0.0, V_LS629_OUT);
    // Q3 chops an RC network, the same shape as Donkey Kong's jump and stomp
    // even though what feeds it is different. The one-shot gates the network and
    // the oscillator modulates it.
    let jump_shot_inv = invert_logic(&mut b, "JUMP_SHOT_INV", jump_shot);
    let q3 = b.rc_disc_modulated(
        "Q3",
        jump_shot_inv,
        tone_8l_v,
        120.0,
        R27,
        1.0,
        R28,
        C28,
        VCC,
    );
    // Output network, read off the drawing as series against shunt: a coupling
    // capacitor, then a series resistor into a shunt capacitor. R4 is both the
    // coupling network's load and this voice's mixer leg, which is one resistor
    // doing both jobs rather than two.
    let jump_ac = b.rc_high_pass("JUMP_AC", q3, R4, C23);
    let jump = b.rc_low_pass("JUMP_OUT", jump_ac, R19, C21);

    // -----------------------------------------------------------------------
    // Climbing (6H bit 2)
    // -----------------------------------------------------------------------
    //
    // The noise clock is 7P pin 10 with its frequency control tied to ground, so
    // it free-runs: the parts alone decide its rate, and the model puts it at
    // 690 Hz against the 710 Hz the part measures. Clocking the register from
    // the oscillator rather than from that number is what keeps the two tied
    // together.
    let fc_7p_b = b.constant("FC_7P_B", 0.0);
    let vco_7p_b = b.ls629_vco(
        "VCO_7P_B",
        fc_7p_b,
        Ls629 {
            c: C20,
            r_freq: 0.0,
            c_freq_in: 0.0,
            v_rng: VCC,
            r_rng: R34,
        },
    );
    let noise = b.lfsr_noise_clocked("NOISE_3J_4J", vco_7p_b, climb_lfsr());
    let noise_v = b.logic_levels("NOISE_V", noise, 0.0, V_LS629_OUT);
    let climb_shot = b.ls123("CLIMB_SHOT", climb_en, LS123_CHARGE, R17, C27);
    let climb_shot_inv = invert_logic(&mut b, "CLIMB_SHOT_INV", climb_shot);
    let q2 = b.rc_disc_modulated(
        "Q2",
        climb_shot_inv,
        noise_v,
        120.0,
        R24,
        1.0,
        R18,
        C29,
        VCC,
    );
    // Same output topology as jump's, three decades apart in corner: 1.3 kHz
    // here against jump's 28 kHz. Jump's is a snubber that does nothing audible;
    // this one genuinely rolls the noise off, and guessing the order would have
    // swapped a tone control for a snubber on both.
    let climb_ac = b.rc_high_pass("CLIMB_AC", q2, R6, C30);
    let climb = b.rc_low_pass("CLIMB_OUT", climb_ac, R2, C25);

    // -----------------------------------------------------------------------
    // Falling (5H bit 1)
    // -----------------------------------------------------------------------
    //
    // The simplest voice and the one nothing reached before: an enable, one
    // inverter, one oscillator half and one NAND, with no one-shot anywhere.
    // The enable does two jobs. It pulls the oscillator's control node down
    // through R14, and C26 is large enough that the pitch takes about a second
    // to slide from ~2.1 kHz to ~570 Hz; and it opens the NAND, so the sound
    // exists only while the line is held. Measured against the game, that line
    // is held for 1.4 s, which is most of one slide.
    let fc_7p_a = b.logic_levels("FC_7P_A", fall_en, V_7N_FALL.1, V_7N_FALL.0);
    let vco_7p_a = b.ls629_vco(
        "VCO_7P_A",
        fc_7p_a,
        Ls629 {
            c: C37,
            r_freq: R14,
            c_freq_in: C26,
            v_rng: VCC,
            r_rng: R34,
        },
    );
    let tone_7p_a = b.variable_square("TONE_7P_A", vco_7p_a);
    let fall_nand = b.logic_gate("NAND_5N_8", LogicOp::Nand, fall_en, tone_7p_a);
    let fall = b.logic_levels("FALL_OUT", fall_nand, V_5N.0, V_5N.1);

    // -----------------------------------------------------------------------
    // DAC (identical to Donkey Kong's)
    // -----------------------------------------------------------------------
    let dac_decay = b.rc_envelope("DAC_DECAY", discharge, DAC_DECAY_S, 0.0);
    let dac_open = b.gain("DAC_OPEN", dac_decay, -1.0);
    let dac_one = b.constant("DAC_ONE", 1.0);
    let dac_gate = b.add("DAC_GATE", &[dac_one, dac_open]);
    let dac_gated = b.multiply("DAC_GATED", dac, dac_gate);
    let dac_lp = b.second_order(
        "DAC_LP",
        dac_gated,
        FilterMode::LowPass,
        DAC_LP_HZ,
        DAC_LP_Q,
    );

    // -----------------------------------------------------------------------
    // Mixer and amplifier
    // -----------------------------------------------------------------------
    //
    // The leg resistors are the board's balance between the voices, so nothing
    // downstream needs a per-voice gain. Climbing's 20 kΩ makes it the loudest
    // by a wide margin and falling's 150 kΩ the quietest, which is the board's
    // judgement rather than ours.
    let mix = b.resistor_mixer(
        "MIX",
        &[
            (fall, R5),
            (walk, R3),
            (climb, R6),
            (jump, R4),
            (dac_lp, R25),
        ],
        None,
    );
    // The summing node's own shunt capacitor, and it is the single largest thing
    // this model was missing.
    //
    // Every source on this board is a logic gate, so every voice arrives as a
    // hard square with nothing rolling its edges off. Compared against the
    // reference, all five captures had their fundamental right to within 2-10 %
    // and their spectral centroid two to four times too high — the same error,
    // in the same direction, on voices with no source part in common. One error
    // shared by five independent voices is downstream of all of them.
    //
    // The corner is not a choice: the capacitor works against the five mixer
    // legs in parallel, which is 9.2 kΩ, so it lands at 1738 Hz whatever anyone
    // would prefer.
    //
    // PROVENANCE, and it is weaker than the rest of this file. The capacitor is
    // not in `docs/schematics/dkongjr-sound-sources.md`: that reading covered
    // the four voices and stopped at the mixer's leg resistors, and this sits
    // one node further on. Its value comes from an independent netlist of the
    // same board rather than from the drawing, so it wants confirming on sheet 5
    // before it is described as read. What makes it a part rather than a fitted
    // filter is that its corner falls out of resistors already transcribed here,
    // and that it is one capacitor explaining five voices at once.
    let mix_lp = b.rc_low_pass("MIX_LP", mix, MIX_R_PARALLEL, C_MIX);
    // The amplifier's input coupling, and the only thing removing the DC that
    // every voice rests at. Each idle voice sits at a gate's high level, not at
    // zero, so without this the mix carries a large pedestal.
    let out = b.rc_high_pass("AMP_IN", mix_lp, AMP_R, C13);
    b.output(out, OutputGain::linear(OUTPUT_GAIN));

    let circuit = b.build();
    (
        circuit,
        DkongJrInputs {
            dac,
            walk_en,
            jump_en,
            climb_en,
            walk_pitch,
            fall_en,
            discharge,
            mix,
        },
    )
}

/// Donkey Kong Jr. discrete sound: the DAC stream summed with the walking,
/// jump, climbing and falling effects inside a [`DiscreteCircuit`].
#[derive(Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct DkongJrDiscreteSound {
    #[save(id = 3)]
    circuit: DiscreteCircuit,
    /// Input handles, fixed when the circuit is built.
    #[save_skip]
    ids: DkongJrInputs,
    /// 74LS259 sound control latch at 6H, bits 0-2 and 7.
    #[save(id = 1)]
    latch_6h: u8,
    /// 74LS259 latch at 5H, bit 1 only — the rest is video.
    #[save(id = 4)]
    latch_5h: u8,
    #[save(id = 2)]
    discharge: bool,
}

impl DkongJrDiscreteSound {
    pub fn new() -> Self {
        let (circuit, ids) = build_circuit();
        Self {
            circuit,
            ids,
            latch_6h: 0,
            latch_5h: 0,
            discharge: false,
        }
    }

    /// Set a bit of the 6H sound-control latch (0 = walking, 1 = jump,
    /// 2 = climbing, 7 = walking pitch). Other bits reach the sound CPU rather
    /// than this circuit.
    pub fn write_sound_bit(&mut self, bit: u8, value: bool) {
        if value {
            self.latch_6h |= 1 << bit;
        } else {
            self.latch_6h &= !(1 << bit);
        }
        match bit {
            0 => self.circuit.set_logic(self.ids.walk_en, value),
            1 => self.circuit.set_logic(self.ids.jump_en, value),
            2 => self.circuit.set_logic(self.ids.climb_en, value),
            7 => self.circuit.set_logic(self.ids.walk_pitch, value),
            _ => {}
        }
    }

    /// Set a bit of the 5H latch. Only bit 1, the falling voice's enable,
    /// reaches this circuit; the others carry video signals and the sound CPU's
    /// interrupt.
    pub fn write_latch_5h_bit(&mut self, bit: u8, value: bool) {
        if value {
            self.latch_5h |= 1 << bit;
        } else {
            self.latch_5h &= !(1 << bit);
        }
        if bit == 1 {
            self.circuit.set_logic(self.ids.fall_en, value);
        }
    }

    /// Feed one box-filtered I8035 DAC sample and advance the circuit by one
    /// output sample's worth of simulation.
    pub fn feed_dac(&mut self, sample: i16) {
        self.circuit
            .set_external(self.ids.dac, sample as f64 / 32767.0 * DAC_V);
        self.circuit.tick(1);
    }

    /// Set the DAC's signal-decay control line.
    pub fn set_discharge(&mut self, value: bool) {
        self.discharge = value;
        self.circuit.set_logic(self.ids.discharge, value);
    }

    /// Drain produced mono `i16` samples. Returns the number written.
    pub fn fill_audio(&mut self, out: &mut [i16]) -> usize {
        self.circuit.fill_audio(out)
    }

    /// The built circuit, for tooling that reads individual stages. A mixed sum
    /// cannot say which of four overlapping voices is wrong.
    pub fn circuit(&self) -> &DiscreteCircuit {
        &self.circuit
    }

    pub fn reset(&mut self) {
        self.circuit.reset();
        self.latch_6h = 0;
        self.latch_5h = 0;
        self.discharge = false;
    }
}

impl Default for DkongJrDiscreteSound {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::tkg04::Tkg04Sound for DkongJrDiscreteSound {
    fn write_sound_bit(&mut self, bit: u8, value: bool) {
        self.write_sound_bit(bit, value);
    }
    fn set_discharge(&mut self, value: bool) {
        self.set_discharge(value);
    }
    fn feed_dac(&mut self, sample: i16) {
        self.feed_dac(sample);
    }
    fn fill_audio(&mut self, out: &mut [i16]) -> usize {
        self.fill_audio(out)
    }
    fn reset(&mut self) {
        self.reset();
    }
}

impl phosphor_core::device::Device for DkongJrDiscreteSound {
    fn name(&self) -> &'static str {
        "DK Jr Discrete"
    }
    fn reset(&mut self) {
        self.reset();
    }
}

impl Debuggable for DkongJrDiscreteSound {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        let bit6h = |b: u8| ((self.latch_6h >> b) & 1) as u64;
        let sample = |v: f64| (v.clamp(-1.0, 1.0) * 32767.0) as i16 as u16 as u64;
        vec![
            DebugRegister {
                name: "LATCH6H",
                value: self.latch_6h as u64,
                width: 8,
            },
            DebugRegister {
                name: "LATCH5H",
                value: self.latch_5h as u64,
                width: 8,
            },
            DebugRegister {
                name: "WALK",
                value: bit6h(0),
                width: 8,
            },
            DebugRegister {
                name: "JUMP",
                value: bit6h(1),
                width: 8,
            },
            DebugRegister {
                name: "CLIMB",
                value: bit6h(2),
                width: 8,
            },
            DebugRegister {
                name: "PITCH",
                value: bit6h(7),
                width: 8,
            },
            DebugRegister {
                name: "FALL",
                value: ((self.latch_5h >> 1) & 1) as u64,
                width: 8,
            },
            DebugRegister {
                name: "DISCHARGE",
                value: self.discharge as u64,
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

    /// Run `n` output samples with a silent DAC, so only the analog effect path
    /// is exercised.
    fn run(s: &mut DkongJrDiscreteSound, n: usize) {
        for _ in 0..n {
            s.feed_dac(0);
        }
    }

    fn drain_rms(s: &mut DkongJrDiscreteSound) -> f64 {
        let mut buf = vec![0i16; 1 << 17];
        let n = s.fill_audio(&mut buf);
        if n == 0 {
            return 0.0;
        }
        let sum: f64 = buf[..n].iter().map(|&v| (v as f64).powi(2)).sum();
        (sum / n as f64).sqrt()
    }

    /// Settle the analog state and throw the result away.
    ///
    /// Every voice idles at a logic gate's output level rather than at zero, so
    /// power-on puts a step through the amplifier's coupling capacitor that has
    /// nothing to do with any voice. Three seconds is several time constants of
    /// the slowest network here (C29 and C28, 10 µF into 100 kΩ). Measuring
    /// across it instead reported an idle "noise floor" of 838 RMS on a board
    /// whose settled idle output is exactly zero.
    fn settle(s: &mut DkongJrDiscreteSound) {
        run(s, 132_300);
        let mut discard = vec![0i16; 1 << 20];
        while s.fill_audio(&mut discard) > 0 {}
    }

    /// Pulse one 6H latch bit for three frames, which is what the game does, and
    /// measure what comes out over half a second.
    fn pulse_6h(bit: u8) -> f64 {
        let mut s = DkongJrDiscreteSound::new();
        settle(&mut s);
        s.write_sound_bit(bit, true);
        run(&mut s, 44_100 * 3 / 60);
        s.write_sound_bit(bit, false);
        run(&mut s, 44_100);
        drain_rms(&mut s)
    }

    /// An untriggered board is SILENT, not merely quiet.
    ///
    /// Worth its own test because three of this board's sources never stop: the
    /// 4020 counts, the noise register shifts, and two oscillators free-run,
    /// with no enable pin anywhere to switch them off. What keeps them inaudible
    /// is the gating downstream — a NAND held closed, a transistor's network
    /// settled — and getting any of that wrong leaks a source into every second
    /// of play rather than failing outright.
    ///
    /// It is also what makes the per-voice checks below mean something: they are
    /// measured against exact zero.
    #[test]
    fn an_untriggered_board_is_silent() {
        let mut s = DkongJrDiscreteSound::new();
        settle(&mut s);
        run(&mut s, 44_100);
        let mut buf = vec![0i16; 1 << 20];
        let n = s.fill_audio(&mut buf);
        assert!(n > 40_000, "expected about a second of audio, got {n}");
        let peak = buf[..n].iter().map(|v| v.abs()).max().unwrap();
        assert_eq!(peak, 0, "an idle board put out a peak of {peak}");
    }

    #[test]
    fn each_latch_bit_drives_its_own_voice() {
        // The check that the latch mapping is right, and it can fail two ways: a
        // bit wired to nothing gives exact silence, and a bit wired to the wrong
        // input gives the wrong voice's level. Both are mistakes this board
        // invites, since the same latch drove a different circuit until now.
        //
        // Measured RMS over the second after a three-frame trigger, which is the
        // pulse the game gives. The ordering is the mixer's — climbing's leg is
        // 20 kΩ against walking's 100 kΩ — so it is the board's balance rather
        // than a judgement here.
        //
        // Both corrections this model needed moved these, and the pattern each
        // left is the evidence that it was the right correction rather than a
        // number that happened to help.
        //
        // The summing node's capacitor moved them by very different amounts —
        // walking down 51 %, jump 11 %, climbing 2 % — which is what a 1738 Hz
        // corner should do to voices whose centroids were 6.6 kHz, 1.3 kHz and
        // 340 Hz. The one-shot width then moved walking down another 28 % and
        // the other two by 2 % and 0.1 %, because walking's is the only one-shot
        // short enough for its width to be most of the sound; jump's and
        // climbing's notes are set by their RC decays, which the one-shot only
        // starts.
        let measured: Vec<(u8, &str, f64, f64)> = [
            (0u8, "walking", 367.0),
            (1, "jump", 1921.0),
            (2, "climbing", 3281.0),
        ]
        .into_iter()
        .map(|(bit, what, want)| (bit, what, want, pulse_6h(bit)))
        .collect();
        // Report every voice before failing on any: one run should say which of
        // the four moved, not just the first.
        for (bit, what, want, driven) in &measured {
            println!("6H bit {bit} ({what}): {driven:8.1} RMS, expected about {want:.0}");
        }
        for (bit, what, want, driven) in &measured {
            assert!(
                (driven - want).abs() < want * 0.2,
                "6H bit {bit} ({what}) produced {driven:.1} RMS, expected about {want:.0}"
            );
        }
    }

    #[test]
    fn falling_is_a_level_and_not_a_trigger() {
        // The one voice on this board with no one-shot: its NAND is opened by
        // the enable itself, so it sounds for exactly as long as the line is
        // held. The game holds it for 86 frames. A model that put a one-shot
        // here — which is what the other three voices would suggest — would pass
        // the "is it audible" check above and fail this one.
        let mut held = DkongJrDiscreteSound::new();
        settle(&mut held);
        held.write_latch_5h_bit(1, true);
        run(&mut held, 44_100);
        let held_rms = drain_rms(&mut held);

        let mut pulsed = DkongJrDiscreteSound::new();
        settle(&mut pulsed);
        pulsed.write_latch_5h_bit(1, true);
        run(&mut pulsed, 44_100 * 3 / 60);
        pulsed.write_latch_5h_bit(1, false);
        run(&mut pulsed, 44_100 - 44_100 * 3 / 60);
        let pulsed_rms = drain_rms(&mut pulsed);

        assert!(
            held_rms > 500.0,
            "holding the falling enable produced only {held_rms:.1} RMS"
        );
        assert!(
            held_rms > pulsed_rms * 3.0,
            "falling should follow its enable: held {held_rms:.1} against pulsed {pulsed_rms:.1}"
        );
    }

    #[test]
    fn the_walking_pitch_bit_changes_the_walking_voice() {
        // 6H bit 7 picks which pair of 4020 taps walking uses, so the same
        // trigger has to make two different sounds. Compared by spectral centroid
        // rather than by level, because the bit is a pitch select and a level
        // comparison would pass on a model that ignored it and merely got louder.
        fn centroid(with_pitch: bool) -> f64 {
            let mut s = DkongJrDiscreteSound::new();
            s.write_sound_bit(7, with_pitch);
            run(&mut s, 22_050);
            let mut buf = vec![0i16; 1 << 17];
            s.fill_audio(&mut buf);
            s.write_sound_bit(0, true);
            run(&mut s, 44_100 * 3 / 60);
            s.write_sound_bit(0, false);
            run(&mut s, 22_050);
            let n = s.fill_audio(&mut buf);
            // Mean absolute sample-to-sample step over RMS: a cheap, robust
            // stand-in for centre frequency that needs no transform.
            let rms = (buf[..n].iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / n as f64).sqrt();
            let steps: f64 = buf[..n]
                .windows(2)
                .map(|w| (w[1] as f64 - w[0] as f64).abs())
                .sum::<f64>()
                / (n - 1) as f64;
            steps / rms.max(1.0)
        }
        let low = centroid(false);
        let high = centroid(true);
        assert!(
            (low - high).abs() / low.max(high) > 0.05,
            "bit 7 moved the walking voice's brightness by less than 5 %: {low:.4} against {high:.4}"
        );
    }

    /// THE CLIMBING REGISTER IS NOT MAXIMAL, AND THAT IS THE BOARD'S.
    ///
    /// A shift register with the wrong direction or the wrong taps does not
    /// fail, it runs a shorter polynomial, and a short cycle is a tone wearing a
    /// noise source's name. So this is asserted rather than assumed — and the
    /// answer was not the expected one, which is why the number is written down
    /// instead of a `(1 << 16) - 1`.
    ///
    /// The board taps bit 2 and bit 15, which is `x^16 + x^13 + 1`, and that
    /// polynomial is not primitive: it factors, and the register's period is the
    /// least common multiple of its factors' periods, 7 and 8191, giving 57337.
    /// Both of those are `2^k - 1` for k = 3 and 13, which is the cross-check
    /// that the shortfall is arithmetic rather than a modelling error.
    ///
    /// It is not audible as a repeat. Clocked at the 690 Hz its oscillator
    /// free-runs at, 57337 states take 83 seconds, and no climb lasts that long.
    /// The reason to pin it is that a genuinely broken register would land far
    /// below this, not just short of maximal.
    #[test]
    fn the_climbing_noise_register_runs_a_long_cycle() {
        let cycle = climb_lfsr().cycle_length();
        assert_eq!(cycle, 57_337);
        assert_eq!(cycle, 7 * 8_191);
    }

    #[test]
    fn save_load_round_trips_mid_effect() {
        let mut a = DkongJrDiscreteSound::new();
        a.write_sound_bit(0, true);
        a.write_sound_bit(7, true);
        a.write_latch_5h_bit(1, true);
        run(&mut a, 5_000);

        let mut w = StateWriter::new();
        a.save_state(&mut w);
        let data = w.into_vec();

        let mut b = DkongJrDiscreteSound::new();
        let mut r = StateReader::new(&data);
        b.load_state(&mut r).unwrap();

        // Drain both so the comparison is of freshly produced audio rather than
        // of whatever each resampler happened to be holding.
        let mut discard = vec![0i16; 1 << 17];
        while a.fill_audio(&mut discard) > 0 {}
        while b.fill_audio(&mut discard) > 0 {}

        run(&mut a, 4_000);
        run(&mut b, 4_000);
        let mut sa = vec![0i16; 1 << 17];
        let mut sb = vec![0i16; 1 << 17];
        let na = a.fill_audio(&mut sa);
        let nb = b.fill_audio(&mut sb);
        assert_eq!(na, nb);
        assert_eq!(sa[..na], sb[..nb]);
    }
}
