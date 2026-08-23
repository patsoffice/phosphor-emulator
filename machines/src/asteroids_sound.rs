//! Asteroids (1979) discrete sound, built on the [`DiscreteCircuit`] framework.
//!
//! Seven effect paths — explosion, thump, saucer, saucer-fire, ship-fire,
//! thrust, and life — summed into one mono output, then through the coupling
//! capacitor at the amplifier's input. The board talks to it with hardware
//! intent (`write_explosion`, `write_thump`, `write_audio_latch_bit`,
//! `pulse_noise_reset`) and never sees internal node ids.
//!
//! Thrust, thump and both fire voices are built stage for stage from Sheet 2
//! Side B, with every component named for its designator at the call site. The
//! saucer's warble and the explosion's pitch divider are not: those still carry
//! literals taken from the reference netlist, and they are the two left to do.
//! Relative mix levels are the reference's adder weights throughout, which is a
//! board-wide question — the schematic gives all seven summing resistors, so
//! expressing every voice in volts and letting the mixer weight them would
//! retire the last constant in the file.

use phosphor_core::core::debug::{DebugRegister, Debuggable};
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::device::{
    CustomComponent, DataInputId, DiscreteCircuit, DiscreteCircuitBuilder, Feed555, LfsrOutput,
    LfsrShift, LfsrSpec, LogicInputId, NodeId, Output555, OutputGain, PulseInputId,
};

use crate::atari_dvg::TIMING;

/// MAME's pitch-divider mapping for the explosion path: register bits 6-7 select
/// the noise re-clock divider (`12 kHz / divider`).
fn explosion_divider(reg: u8) -> f64 {
    match (reg >> 6) & 0x03 {
        0 => 12.0,
        1 => 6.0,
        2 => 3.0,
        _ => 5.0,
    }
}

// ---------------------------------------------------------------------------
// Explosion noise generator (custom escape-hatch component)
// ---------------------------------------------------------------------------

/// 16-bit XNOR LFSR re-clocked at `12 kHz / divider`, scaled by volume. This is
/// circuit-specific (variable re-clock rate + register-cleared reset), so it
/// rides the framework's `Custom` escape hatch rather than a shared primitive.
///
/// Inputs: `[volume 0..1, divider, noise_reset 0/1]`.
struct ExplosionNoise {
    lfsr: u16,
    clock_acc: f64,
}

impl ExplosionNoise {
    // Reset/seed value 0, matching MAME. For an XNOR LFSR the *all-ones* state is
    // the lock state (it feeds back ones forever); 0 is the natural running seed.
    const SEED: u16 = 0;
}

impl CustomComponent for ExplosionNoise {
    fn reset(&mut self) {
        self.lfsr = Self::SEED;
        self.clock_acc = 0.0;
    }

    fn step(&mut self, inputs: &[f64], dt: f64) -> f64 {
        let volume = inputs[0];
        let divider = inputs[1].max(1.0);
        if inputs[2] > 0.5 {
            // Noise reset clears the register (XNOR feedback recovers from 0).
            self.lfsr = 0;
        }
        let freq = 12_000.0 / divider;
        self.clock_acc += freq * dt;
        while self.clock_acc >= 1.0 {
            self.clock_acc -= 1.0;
            let fb = !(((self.lfsr >> 6) ^ (self.lfsr >> 14)) & 1) & 1;
            self.lfsr = (self.lfsr << 1) | fb;
        }
        let level = if self.lfsr & 1 != 0 { 1.0 } else { -1.0 };
        level * volume
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.write_u16_le(self.lfsr);
        w.write_f64_le(self.clock_acc);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.lfsr = r.read_u16_le()?;
        self.clock_acc = r.read_f64_le()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Fire voice storage capacitors (custom escape-hatch components)
// ---------------------------------------------------------------------------

/// The fire voices' pitch capacitor: C38 with Q3 (saucer) and C47 with Q1
/// (ship).
///
/// A 4016B section holds the cap at +5 V while the enable is low. Releasing it
/// leaves a PNP constant-current source charging the cap, and that rising
/// voltage is what a following op-amp turns into the 555's falling charge
/// current. So the pitch does not sweep because something ramps it; it sweeps
/// because a capacitor is filling, and it stops when the transistor saturates.
///
/// Input: `[enable 0/1]`. Output: the capacitor's voltage.
struct FireControlCap {
    /// What the analog switch holds the cap at while the voice is idle.
    hold_v: f64,
    /// Where the PNP saturates and stops sourcing, which is what ends the
    /// sweep. Nothing else on this node draws current, so this is a floor the
    /// voltage rests on rather than an asymptote it approaches.
    ceiling_v: f64,
    /// `i/C` in volts per second.
    slew: f64,
    v: f64,
}

impl FireControlCap {
    fn new(hold_v: f64, ceiling_v: f64, current: f64, c: f64) -> Self {
        Self {
            hold_v,
            ceiling_v,
            slew: current / c,
            v: hold_v,
        }
    }
}

impl CustomComponent for FireControlCap {
    fn reset(&mut self) {
        self.v = self.hold_v;
    }

    fn step(&mut self, inputs: &[f64], dt: f64) -> f64 {
        if inputs[0] > 0.5 {
            self.v = (self.v + self.slew * dt).min(self.ceiling_v);
        } else {
            self.v = self.hold_v;
        }
        self.v
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.write_f64_le(self.v);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.v = r.read_f64_le()?;
        Ok(())
    }
}

/// The fire voices' output network: C39/R58/CR6 (saucer) and C48/R66/CR8
/// (ship), summed into the mixer through R81/R84.
///
/// This is where the amplitude decay lives, and it is not an envelope
/// multiplying an oscillator. A second 4016B section holds this capacitor at
/// +5 V while the voice is idle. Once released, the capacitor's voltage reaches
/// the summing node through the series resistor, and the 555 is tied to that
/// node through a diode whose **cathode faces the timer**. So the timer can
/// only ever pull the node down, and only in the half of its cycle where its
/// output transistor is sinking; the level it is pulled up from is whatever
/// charge the capacitor has left.
///
/// Three things follow that a multiplied envelope does not give:
///
/// - The decay rate depends on the duty cycle, because the capacitor empties
///   through the diode during the low phase and through the series resistor and
///   the summing resistor into the mixer's +5 V reference during the high one,
///   and those differ by a factor of eleven on the saucer.
/// - The decay therefore does not reach zero. It settles where the two average
///   out, which is why the reference's own amplitude ramp stops at a floor it
///   adds by hand.
/// - The waveform is a *pulse*, and its rest is the clamp rather than the
///   reference. An idle voice has its 555 held in reset, so pin 3 is low and the
///   diode is conducting: the node sits at the clamp all the while, and firing
///   is the node being *released upward* for the fraction of each cycle the
///   timer spends charging. That fraction starts near 62 % and grows toward 94 %
///   as the pitch falls, so the voice ends as a narrow notch train rather than
///   fading out.
///
/// Inputs: `[enable 0/1, 555 output volts]`. Output: how far the summing node
/// stands above where it rests, in volts.
struct FireOutput {
    /// Series resistor from the capacitor to the summing node (R58 / R66).
    r_series: f64,
    /// Summing resistor into the mixer's virtual ground (R81 / R84).
    r_mix: f64,
    /// Storage capacitor (C39 / C48).
    c: f64,
    /// What the analog switch holds the capacitor at while idle, which is also
    /// the voltage the mixer holds its summing node at.
    hold_v: f64,
    /// Where the diode pins the node while the timer's output is low, which is
    /// also where an idle voice rests: the enable drives the 555's reset, and a
    /// 555 in reset has its output low.
    clamp_v: f64,
    v: f64,
}

impl FireOutput {
    fn new(r_series: f64, c: f64, r_mix: f64, hold_v: f64, clamp_v: f64) -> Self {
        Self {
            r_series,
            r_mix,
            c,
            hold_v,
            clamp_v,
            v: hold_v,
        }
    }
}

impl CustomComponent for FireOutput {
    fn reset(&mut self) {
        self.v = self.hold_v;
    }

    fn step(&mut self, inputs: &[f64], dt: f64) -> f64 {
        if inputs[0] <= 0.5 {
            // The switch closes and the capacitor is back at the rail. Snapping
            // it rather than charging it through the 4016B's own few hundred
            // ohms is a 2 ms approximation of a 2 ms event, and it is
            // unobservable twice over: the node reads the clamp throughout,
            // because the timer is in reset with its output low, and the game's
            // own fire timer cannot re-trigger inside one frame.
            self.v = self.hold_v;
            return 0.0;
        }
        // What the node would sit at with the diode out of circuit: the
        // capacitor and the mixer's reference, divided by their two resistors.
        let (g_series, g_mix) = (1.0 / self.r_series, 1.0 / self.r_mix);
        let open = (self.v * g_series + self.hold_v * g_mix) / (g_series + g_mix);
        // The timer sinks through the diode only while its output is low. It is
        // NOT clamped while high, even when the node sits above the timer's own
        // high level: a 555's high side is a Darlington to Vcc that turns off
        // rather than sinking, so there is nothing for a forward diode current
        // to flow into. Modelling a clamp there would be inventing a mechanism
        // the part does not have, which is the failure this file keeps finding.
        let node = if inputs[1] > 0.5 {
            open
        } else {
            open.min(self.clamp_v)
        };
        self.v -= (self.v - node) / self.r_series * dt / self.c;
        node - self.clamp_v
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.write_f64_le(self.v);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.v = r.read_f64_le()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Typed input handles + node ids for debug
// ---------------------------------------------------------------------------

struct AsteroidsDiscreteInputs {
    explosion_vol: DataInputId,
    explosion_pitch: DataInputId,
    noise_reset: PulseInputId,
    thump_en: LogicInputId,
    thump_data: DataInputId,
    saucer_en: LogicInputId,
    saucer_fire: LogicInputId,
    saucer_sel: LogicInputId,
    thrust_en: LogicInputId,
    ship_fire: LogicInputId,
    life_en: LogicInputId,
    mix: NodeId,
}

// ---------------------------------------------------------------------------
// Circuit construction
// ---------------------------------------------------------------------------

/// Relative mix levels from MAME's final adder (`asteroid_a.cpp`). Normalized so
/// the summed output stays within [-1, 1].
const LVL_EXPLOSION: f64 = 1000.0;
const LVL_THRUST: f64 = 600.0;
const LVL_LIFE: f64 = 100.0;
const LVL_THUMP: f64 = 131.6;
const LVL_SAUCER: f64 = 76.1;
const LVL_SHIP_FIRE: f64 = 53.0;
const LVL_SAUCER_FIRE: f64 = 49.5;
const LVL_TOTAL: f64 = LVL_EXPLOSION
    + LVL_THRUST
    + LVL_LIFE
    + LVL_THUMP
    + LVL_SAUCER
    + LVL_SHIP_FIRE
    + LVL_SAUCER_FIRE;

/// Thrust band-pass: a multiple-feedback stage around one section of the LM324.
///
/// R76 carries the signal into the node and R100 ties that node to the +5 V
/// reference, so BOTH belong in the builder's `r_in` list. The centre frequency
/// and Q come from their parallel combination, 47 k ∥ 1.2 k = 1170 Ω, while the
/// centre gain is `Rf/(2·R76)`, or 2.87.
///
/// This used to pass the parallel value alone, as a single 1170 Ω input
/// resistor. That gets the first two exactly right and the third wrong by
/// 47 k / 1170 = 40, so the filter's shape measured perfectly while its gain was
/// forty times what the parts give. Nothing that looks at a corner or a Q could
/// have caught it, and the missing factor sat in a fitted output gain instead.
const THRUST_BP_R_IN: f64 = 47_000.0; // R76
const THRUST_BP_R_REF: f64 = 1_200.0; // R100, to the +5 V reference
const THRUST_BP_RF: f64 = 270_000.0; // R101
const THRUST_BP_C: f64 = 0.1e-6; // C67 and C68

/// Centre gain of the band-pass above, `Rf/(2·R_in)`, which the path's own gain
/// divides back out so the mix weight is the only thing it applies.
const THRUST_BP_CENTRE_GAIN: f64 = THRUST_BP_RF / (2.0 * THRUST_BP_R_IN);

/// Thrust output stage: an inverting active low-pass on the next LM324 section.
///
/// R104 sets the input, R103 the feedback and C69 sits across R103. That is a
/// DC gain of `R103/R104` and a SINGLE pole at `1/(2π·R103·C69)`, or 159 Hz.
///
/// This was a second-order 120 Hz Butterworth with unity gain, added to suppress
/// upper noise the band-pass skirts let through. It had no part behind it, and
/// what it was suppressing was mostly the noise source's own defect: it cost
/// about a quarter of a point of crest factor by rolling off the peaks that make
/// filtered noise sound like noise.
const THRUST_LP_R_IN: f64 = 6_800.0; // R104
const THRUST_LP_RF: f64 = 10_000.0; // R103
const THRUST_LP_C: f64 = 0.1e-6; // C69
const THRUST_LP_GAIN: f64 = THRUST_LP_RF / THRUST_LP_R_IN;

/// Q of the thrust band-pass, which is also its energy make-up factor.
///
/// A band-pass this narrow throws away most of the noise handed to it, so a path
/// carrying its mix weight through one arrives far too quiet. Multiplying the
/// weight by Q is the compensation, and the reference makes the same correction
/// in the same place for the same reason.
const THRUST_Q: f64 = 7.6;

/// Output gain after the thrust band-pass: the path's mix weight, scaled by the
/// filter's energy make-up, with the band-pass primitive's own resonant gain
/// divided back out so it is not counted twice.
///
/// Dividing out the band-pass's centre gain makes this path algebraically the
/// reference's: a weight-times-Q scaling, filters with unity centre gain, and
/// one output normalisation. That is why the two agree on absolute level once
/// the stages either side of it are right.
///
/// STILL NOT DERIVED FROM PARTS, and the distinction matters. The measured level
/// agrees with the board to well inside the window-to-window spread, but the
/// weight is the reference's own tweaked 600 rather than the 1000 its summing
/// resistor implies, and Q is here as an energy correction rather than as a
/// component. What would retire it is expressing every voice in volts and
/// letting the mixer weight them, which the schematic now makes possible:
/// thrust enters the LM324 mixer through R102 4.7 kΩ against R86's 1 kΩ, the
/// same pair the explosion uses. That is a board-wide change, not a thrust one.
///
/// It was 0.12, fitted against a reference capture, and that value was standing
/// in for the broken noise source upstream: a ringing filter needs far more
/// make-up than a genuinely excited one, so once the register was corrected the
/// old gain drove the path into hard clipping at full scale. That is what a gain
/// fitted over a broken stage does.
const THRUST_GAIN: f64 = (LVL_THRUST / LVL_TOTAL) * THRUST_Q / THRUST_BP_CENTRE_GAIN;

/// Half the 555's output swing, which is the amplitude its square carries once
/// the amplifier's coupling capacitor has centred it. `out_high` is `vcc − 1.2`.
const THUMP_555_SWING: f64 = (5.0 - 1.2) / 2.0;

/// Output gain after the thump 555/RC chain: the path's mix weight, applied to a
/// signal first normalised out of volts, which is the same shape thrust uses.
///
/// This was 0.135, "calibrated to the reference thump level", and it was fitted
/// against the capacitor tap. With the square, which is what the board takes, it
/// ran the voice 10 dB hot.
///
/// The residual is about a decibel, and it is the reference's own doing: its
/// thump carries a bare `GAIN 30` where the mix level its table documents would
/// want 69. That is the same kind of tweak as thrust's 600 against the 1000 its
/// summing resistor implies, and it is not something to reproduce.
const THUMP_GAIN: f64 = (LVL_THUMP / LVL_TOTAL) / THUMP_555_SWING;

// ---------------------------------------------------------------------------
// Fire voices — the two "pew" chains, built from the board's own parts
// ---------------------------------------------------------------------------
//
// The board draws these twice, identically, and the manual says so: "The Fire
// sounds for the Saucer and the Space Ships are generated by two identical
// circuits." Only two components differ between them, and those two are the
// whole difference in character. Everything below is shared.
//
// The chain is: a 4016B section holds a capacitor at +5 V while the voice is
// idle. On enable it is released and a PNP constant-current source charges it.
// An LM324 follower puts that rising voltage on a second PNP's emitter, whose
// 3.3 kΩ to +12 V therefore delivers a FALLING current, and that current runs a
// 555 as a constant-current VCO. So the pitch sweeps down because a capacitor
// is filling up. In parallel, a second 4016B section releases a second
// capacitor, and that one's decaying charge sets how far the 555 can pull the
// summing node down through its diode. See [`FireOutput`] for why that is not
// an amplitude envelope.
//
// None of this was here before. The model was a linear frequency ramp driving a
// 555 with a 0.01 µF timing cap and no discharge resistor, multiplied by an
// exponential envelope, with both output levels fitted. Every one of those
// numbers is now a part.

/// CR3 and CR4 (1N914) drop this each, at the ~10 mA R53's 1 kΩ draws from
/// +12 V. Note R53 does not set the reference itself — two diodes in series
/// with it fix the node at `12 − 2·Vf` whatever the resistor is — it only sets
/// the current, and so the drop.
const FIRE_BIAS_DIODE_V: f64 = 0.72;
/// 2N3906 base-emitter drop at the tens of microamps these two source.
const FIRE_PNP_VEB: f64 = 0.6;
/// 2N3906 collector-emitter saturation, where Q3/Q1 stop being current sources
/// and the sweep ends.
const FIRE_PNP_VCE_SAT: f64 = 0.2;
const FIRE_SUPPLY_V: f64 = 12.0;
/// What both M9 4016B sections hold their capacitors at while the voice is
/// idle, and also the voltage P11's non-inverting input holds the summing node
/// at. The two being equal is why an idle fire voice contributes exactly zero
/// rather than a bias the mixer has to reject.
const FIRE_REST_V: f64 = 5.0;
/// The reference the two current sources work against: `12 − CR3 − CR4`.
const FIRE_BIAS_V: f64 = FIRE_SUPPLY_V - 2.0 * FIRE_BIAS_DIODE_V;
/// Voltage across R54/R52, which with that resistor is the charging current.
const FIRE_CS_DRIVE_V: f64 = FIRE_SUPPLY_V - FIRE_BIAS_V - FIRE_PNP_VEB;
/// Where the pitch capacitor stops rising: the source transistor's own emitter,
/// less its saturation drop.
const FIRE_CV_CEILING_V: f64 = FIRE_BIAS_V + FIRE_PNP_VEB - FIRE_PNP_VCE_SAT;

/// M8/L9 555 timing parts. R56/R65 is the current-source emitter resistor from
/// +12 V, C35/C50 the timing cap, R57/R61 the resistor between the discharge
/// pin and that cap.
const FIRE_555_R: f64 = 3_300.0; // R56 / R65
const FIRE_555_C: f64 = 1e-6; // C35 / C50
const FIRE_555_R_DISCH: f64 = 680.0; // R57 / R61
const FIRE_555_VCC: f64 = 5.0;

/// R81/R84 into the mixer's 1 kΩ feedback: the highest summing resistor on the
/// board, and the quietest two voices for it.
const FIRE_MIX_R: f64 = 100_000.0; // R81 / R84

/// Where CR6/CR8 pin the summing node while the 555's output is low: the
/// timer's own output-low level plus the 1N914's forward drop at the few
/// hundred microamps R58/R66 hands it.
const FIRE_CLAMP_V: f64 = 0.1 + 0.6;

/// Half the summing node's full swing, from the diode clamp it rests at up to
/// the +5 V it reaches with a full output capacitor. Dividing by it turns the
/// node's volts into the normalised units the mix weights are expressed in,
/// which is the shape thrust and thump use.
const FIRE_HALF_SWING: f64 = (FIRE_REST_V - FIRE_CLAMP_V) / 2.0;

/// The board's noise register: 16 bits, XNOR of bits 6 and 14 fed back into the
/// bottom, clocked at 12 kHz.
///
/// The direction is the whole point. Tap numbers read off a schematic describe a
/// register shifting toward its high end, and running the same numbers the other
/// way is a different polynomial. This was `toward_zero(16, (6, 14), 0xACE1)`,
/// which is not primitive at all in that direction: its longest cycle is 42
/// states from any of the 65536 starting points, so the "noise" repeated every
/// 3.5 ms and was a 286 Hz tone.
///
/// It measured as the right pitch throughout, because the stage after it is a
/// band-pass with a Q of 7.6 and that rings at its own 89.5 Hz whatever it is
/// fed. What it could not do was sound like noise: filtered white noise has a
/// crest factor above 3, and a ringing filter has one near a sine wave's.
///
/// XNOR rather than XOR makes the all-zero state ordinary and the all-ones state
/// the lock, which is why the register can seed at zero, the state it powers up
/// in. The output is the gate's own term rather than a register bit; that is a
/// one-step phase difference and does not change the spectrum.
const THRUST_NOISE_LFSR: LfsrSpec = LfsrSpec {
    width: 16,
    taps: (6, 14),
    seed: 0,
    shift: LfsrShift::TowardHigh,
    invert_feedback: true,
    output: LfsrOutput::Feedback,
};

/// The four components that differ between the two fire voices, and the mix
/// weight that goes with them. Everything else in the chain is shared, so this
/// is the whole of what makes one a "pew" and the other a "pip".
struct FireParts {
    /// Current-source emitter resistor: R54 (saucer) / R52 (ship).
    r_source: f64,
    /// Pitch capacitor: C38 / C47.
    c_pitch: f64,
    /// Output-network series resistor: R58 / R66.
    r_series: f64,
    /// Output-network storage capacitor: C39 / C48.
    c_amp: f64,
    /// The path's weight in the final adder.
    level: f64,
}

/// Build one fire "pew" from the board's parts.
fn build_fire(
    b: &mut DiscreteCircuitBuilder,
    enable: LogicInputId,
    name: &str,
    parts: FireParts,
) -> NodeId {
    let FireParts {
        r_source,
        c_pitch,
        r_series,
        c_amp,
        level,
    } = parts;
    let cv = b.custom(
        &format!("{name}_FIRE_CV"),
        vec![enable.into()],
        Box::new(FireControlCap::new(
            FIRE_REST_V,
            FIRE_CV_CEILING_V,
            FIRE_CS_DRIVE_V / r_source,
            c_pitch,
        )),
    );
    let osc = b.ne555_cc(
        &format!("{name}_FIRE_555"),
        cv,
        // The enable reaches pin 4, the reset, exactly as thump's does. Gating
        // the output instead is what made thump scratch at every onset.
        Some(enable.into()),
        FIRE_555_R,
        FIRE_555_C,
        FIRE_555_R_DISCH,
        FIRE_555_VCC,
        FIRE_SUPPLY_V,
        // No junction drop. Q4/Q5 sit inside an LM324 follower's feedback loop,
        // which holds the emitter at the control voltage itself, so the base-
        // emitter drop the bare current source in thump has to carry is not in
        // this current's equation at all. Putting the transistor's 0.6 V here
        // would be reading the part rather than the circuit.
        0.0,
        // The source is on the discharge pin with R57/R61 between it and the
        // cap, which is the opposite of thump's arrangement a few centimetres
        // away on the same sheet. The cap therefore empties toward ground, not
        // toward i·R57, and that is worth a factor of three in discharge time
        // at the top of the sweep.
        Feed555::DischargePin,
        // Pin 3, the square, which is what CR6/CR8 reads.
        Output555::Square,
    );
    let node = b.custom(
        &format!("{name}_FIRE_NODE"),
        vec![enable.into(), osc],
        Box::new(FireOutput::new(
            r_series,
            c_amp,
            FIRE_MIX_R,
            FIRE_REST_V,
            FIRE_CLAMP_V,
        )),
    );
    b.gain(
        &format!("{name}_FIRE_OUT"),
        node,
        (level / LVL_TOTAL) / FIRE_HALF_SWING,
    )
}

fn build_circuit() -> (DiscreteCircuit, AsteroidsDiscreteInputs) {
    let mut b = DiscreteCircuitBuilder::new(
        TIMING.cpu_clock_hz,
        phosphor_core::audio::host_sample_rate() as u64,
    );

    // --- Board-facing inputs ---
    let explosion_vol = b.data_input("EXPLODE_VOL", 1.0);
    let explosion_pitch = b.data_input("EXPLODE_PITCH", 1.0);
    let noise_reset = b.pulse_input("NOISE_RESET");
    let thump_en = b.logic_input("THUMP_EN");
    let thump_data = b.data_input("THUMP_DATA", 1.0);
    let saucer_en = b.logic_input("SAUCER_EN");
    let saucer_fire = b.logic_input("SAUCER_FIRE");
    let saucer_sel = b.logic_input("SAUCER_SEL");
    let thrust_en = b.logic_input("THRUST_EN");
    let ship_fire = b.logic_input("SHIP_FIRE");
    let life_en = b.logic_input("LIFE_EN");

    // --- Explosion: pitched LFSR noise -> RC low-pass ---
    let expl_noise = b.custom(
        "EXPLODE_NOISE",
        vec![
            explosion_vol.into(),
            explosion_pitch.into(),
            noise_reset.into(),
        ],
        Box::new(ExplosionNoise {
            lfsr: ExplosionNoise::SEED,
            clock_acc: 0.0,
        }),
    );
    let expl_lp = b.rc_low_pass("EXPLODE_LP", expl_noise, 3_042.0, 1e-6);
    let expl = b.gain("EXPLODE", expl_lp, LVL_EXPLOSION / LVL_TOTAL);

    // --- Thrust: 12 kHz noise -> RC pre-filter (~72 Hz) -> gate -> resonant
    // op-amp multiple-feedback band-pass (~89.5 Hz, Q ~7.6) -> output low-pass
    // -> mix weight. The noise source, the pre-filter, the gate and the
    // band-pass all match the reference stage for stage. ---
    let thrust_noise = b.lfsr_noise("THRUST_NOISE", 12_000.0, THRUST_NOISE_LFSR);
    // R75 and C62: one pole at 72.3 Hz.
    let thrust_rc = b.rc_low_pass("THRUST_RC", thrust_noise, 2_200.0, 1e-6);
    // The board gates with a 4016B analog switch BEFORE this RC, not after it,
    // so its attack and release are shaped differently from what this does.
    // Left as is for now: it moves the edges of the effect and nothing in the
    // steady state, which is what the scenario measures.
    let thrust_gated = b.multiply("THRUST_GATE", thrust_rc, thrust_en);
    // fc ≈ 89.5 Hz, Q ≈ 7.6, centre gain 2.87. Both input resistors are listed:
    // the signal enters through the first and the second ties the node to the
    // reference, and the builder needs both to separate the filter's shape from
    // its gain. Rails wide enough to stay linear over the noise drive.
    let thrust_bp = b.op_amp_band_pass(
        "THRUST_BP",
        thrust_gated,
        &[THRUST_BP_R_IN, THRUST_BP_R_REF],
        THRUST_BP_RF,
        THRUST_BP_C,
        THRUST_BP_C,
        0.0,
        -12.0,
        12.0,
    );
    // One pole at 159 Hz, from R103 across C69. The stage's own gain and the
    // path's mix weight are applied together at the node below.
    let thrust_lp = b.rc_low_pass("THRUST_LP", thrust_bp, THRUST_LP_RF, THRUST_LP_C);
    let thrust = b.gain("THRUST", thrust_lp, THRUST_LP_GAIN * THRUST_GAIN);

    // --- Thump: a 4-bit R-1 DAC sets the control voltage of a constant-current
    // 555 VCO (the cap sawtooth), AC-coupled, RC-smoothed and gated. Higher data
    // raises the CV, lowering the charge current and the pitch (~200 Hz at data 0
    // down to ~55 Hz at full data). ---
    // DAC node voltage = (Σ bit·Von/R + Vbias/Rbias) / (Σ1/R + 1/Rbias + 1/Rgnd),
    // with R = 220k/100k/47k/22k (bits 0-3), Von 3.5, Vbias 4.3, Rbias 6.8k,
    // Rgnd 47k.
    let thump_denom =
        1.0 / 220e3 + 1.0 / 100e3 + 1.0 / 47e3 + 1.0 / 22e3 + 1.0 / 6.8e3 + 1.0 / 47e3;
    let thump_weights: Vec<f64> = [220e3, 100e3, 47e3, 22e3]
        .iter()
        .map(|r| 3.5 / (r * thump_denom))
        .collect();
    let thump_dac_bits = b.dac_weighted("THUMP_DAC", thump_data, &thump_weights);
    let thump_dac_off = b.constant("THUMP_DAC_OFF", 4.3 / (6.8e3 * thump_denom));
    let thump_cv = b.add("THUMP_CV", &[thump_dac_bits, thump_dac_off]);
    // No trim on this. It used to pass through a 1.027 gain "to land the
    // reference pitch", and what that was compensating for was the missing
    // discharge time: with the cap snapping back in one step the period was
    // short by however long R51 takes, and slowing the charge by 2.7 % hid it.
    // Model the discharge and the pitch lands on the parts.
    let thump_555 = b.ne555_cc(
        "THUMP_555",
        thump_cv,
        // The enable gates the 555's reset pin, which is where the board puts
        // it. Gating the output instead switched a free-running oscillator on
        // at whatever phase it was passing, and that step was audible as a
        // scratch at every onset.
        Some(thump_en.into()),
        22e3,    // R50, the current-source emitter resistor
        0.22e-6, // C33, the timing cap
        18e3,    // R51 on the discharge pin, which is what gives pin 3 a duty
        5.0,
        5.0,
        0.8, // vcc, v_cc_source, 2N3906 junction
        // Q2's collector joins C33 and R51 goes on to pin 7, so the source is
        // on the capacitor and the cap relaxes toward i·R51 while the pin
        // sinks. The two fire timers below are drawn the other way round.
        Feed555::Capacitor,
        // Pin 3, the square, which is what the board takes through R74. This
        // used to tap the capacitor, because without R51 above the square was a
        // pulse one step wide and unusable. A sawtooth's harmonics fall as 1/n²
        // where a square's fall as 1/n, so the voice measured dull: 17 Hz low on
        // centroid with 7 points too much energy below 150 Hz.
        Output555::Square,
    );
    let thump_rc = b.rc_low_pass("THUMP_RC", thump_555, 3.3e3, 0.1e-6); // R74, C64: 482 Hz
    // No gate here: the 555's reset above is the gate, as on the board. A
    // multiply at this point would also have to cut the RC's stored charge,
    // which is a discontinuity the hardware does not make.
    let thump = b.gain("THUMP", thump_rc, THUMP_GAIN);

    // --- Saucer (MAME asteroid_a.cpp): a triangle warble LFO (8.25 Hz small /
    // 5.75 Hz large) sweeps a triangle tone VCO. SAUCER_SEL shifts both the
    // warble rate and the tone centre: ~750-1670 Hz small, ~500-1420 Hz large.
    // The mellow triangle (not a square) avoids the harsh upper harmonics.
    let warble_base = b.constant("SAUCER_WBASE", 8.25);
    let warble_sel = b.gain("SAUCER_WSEL", saucer_sel, -2.5); // 8.25 small / 5.75 large
    let warble_rate = b.add("SAUCER_WRATE", &[warble_base, warble_sel]);
    let warble_lfo = b.variable_triangle("SAUCER_LFO", warble_rate); // ±1 triangle
    let warble_dev = b.gain("SAUCER_DEV", warble_lfo, 460.0); // ±460 Hz sweep
    let saucer_base = b.constant("SAUCER_BASE", 1_210.0);
    let saucer_seloff = b.gain("SAUCER_SELOFF", saucer_sel, -250.0); // large saucer lower
    let saucer_freq = b.add("SAUCER_FREQ", &[saucer_base, warble_dev, saucer_seloff]);
    let saucer_tone = b.variable_triangle("SAUCER_TONE", saucer_freq);
    let saucer_gated = b.multiply("SAUCER_G", saucer_tone, saucer_en);
    let saucer = b.gain("SAUCER", saucer_gated, LVL_SAUCER / LVL_TOTAL);

    // --- Fire paths: two identical chains, differing in four components. ---
    //
    // Ship: 33 kΩ charging 1 µF is 25 V/s, so its pitch capacitor crosses the
    // whole span in about 0.23 s and the sweep is wide, ~795 Hz down to ~175.
    // Its output capacitor empties through 2.7 kΩ, the fastest decay here.
    let ship_out = build_fire(
        &mut b,
        ship_fire,
        "SHIP",
        FireParts {
            r_source: 33e3,  // R52
            c_pitch: 1e-6,   // C47
            r_series: 2.7e3, // R66
            c_amp: 10e-6,    // C48
            level: LVL_SHIP_FIRE,
        },
    );
    // Saucer: 10 kΩ into 10 µF is 8.4 V/s, twelve times slower on the pitch, so
    // the same 0.28 s covers only ~795 Hz down to ~605. Its output capacitor
    // empties through 10 kΩ, and the two together are why this one is a short
    // "pip" where the ship's is a "pew".
    let sfire_out = build_fire(
        &mut b,
        saucer_fire,
        "SAUCER",
        FireParts {
            r_source: 10e3, // R54
            c_pitch: 10e-6, // C38
            r_series: 10e3, // R58
            c_amp: 10e-6,   // C39
            level: LVL_SAUCER_FIRE,
        },
    );

    // --- Life: fixed 3 kHz tone, gated ---
    let life_tone = b.fixed_square("LIFE_TONE", 3_000.0);
    let life_gated = b.multiply("LIFE_G", life_tone, life_en);
    let life = b.gain("LIFE", life_gated, LVL_LIFE / LVL_TOTAL);

    // --- Final mix ---
    let mix = b.add(
        "MIX",
        &[expl, thrust, thump, saucer, ship_out, sfire_out, life],
    );
    // The amplifier board's input coupling, R14 and C6 on the Regulator/Audio
    // PCB: 1.59 Hz, which is inaudible and is what actually removes the DC this
    // board's voices carry. Thump needs it, because its 555 output is a
    // unipolar square sitting well above ground and the game PCB has nothing in
    // its path to centre it.
    //
    // Modelling it here rather than leaving the DC for the frontend follows the
    // rule this project arrived at over fourteen machines: a DC offset is a
    // missing coupling capacitor, and the fix is the capacitor. The reference
    // instead subtracts half the square's swing, which its own header calls a
    // cheat to make the waveform AC, and which leaves a residual whenever the
    // duty is not 50 %. That is worth not reproducing.
    let out = b.rc_high_pass("AUDIO_COUPLING", mix, 10e3, 10e-6);
    b.output(out, OutputGain::unity());

    let circuit = b.build();
    (
        circuit,
        AsteroidsDiscreteInputs {
            explosion_vol,
            explosion_pitch,
            noise_reset,
            thump_en,
            thump_data,
            saucer_en,
            saucer_fire,
            saucer_sel,
            thrust_en,
            ship_fire,
            life_en,
            mix,
        },
    )
}

// ---------------------------------------------------------------------------
// AsteroidsDiscreteSound — board-facing wrapper
// ---------------------------------------------------------------------------

/// Concrete Asteroids sound device. Wraps a [`DiscreteCircuit`] and exposes
/// hardware-intent methods for the board's bus writes.
pub struct AsteroidsDiscreteSound {
    circuit: DiscreteCircuit,
    ids: AsteroidsDiscreteInputs,
    /// 0x3600 explosion register (volume bits 2-5, pitch bits 6-7).
    explosion_reg: u8,
    /// 0x3A00 thump register (bit 4 enable, low nibble DAC data).
    thump_reg: u8,
    /// 0x3C00-0x3C07 addressable audio latch (74LS259).
    audio_latch: u8,
}

impl AsteroidsDiscreteSound {
    pub fn new() -> Self {
        let (circuit, ids) = build_circuit();
        Self {
            circuit,
            ids,
            explosion_reg: 0,
            thump_reg: 0,
            audio_latch: 0,
        }
    }

    /// 0x3600: explosion. Bits 2-5 are volume (0-15); bits 6-7 the pitch divider.
    pub fn write_explosion(&mut self, data: u8) {
        self.explosion_reg = data;
        let volume = ((data >> 2) & 0x0F) as f64 / 15.0;
        self.circuit.set_data(self.ids.explosion_vol, volume);
        self.circuit
            .set_data(self.ids.explosion_pitch, explosion_divider(data));
    }

    /// 0x3A00: thump. Bit 4 enables; the low nibble is the 4-bit DAC code.
    pub fn write_thump(&mut self, data: u8) {
        self.thump_reg = data;
        self.circuit.set_logic(self.ids.thump_en, data & 0x10 != 0);
        self.circuit
            .set_data(self.ids.thump_data, (data & 0x0F) as f64);
    }

    /// 0x3C00-0x3C07: addressable audio latch (74LS259). `bit` selects the line.
    pub fn write_audio_latch_bit(&mut self, bit: u8, value: bool) {
        crate::set_bit_active_high(&mut self.audio_latch, bit, value);
        match bit {
            0 => self.circuit.set_logic(self.ids.saucer_en, value),
            1 => self.circuit.set_logic(self.ids.saucer_fire, value),
            2 => self.circuit.set_logic(self.ids.saucer_sel, value),
            3 => self.circuit.set_logic(self.ids.thrust_en, value),
            4 => self.circuit.set_logic(self.ids.ship_fire, value),
            5 => self.circuit.set_logic(self.ids.life_en, value),
            _ => {}
        }
    }

    /// 0x3E00: noise reset pulse.
    pub fn pulse_noise_reset(&mut self) {
        self.circuit.pulse(self.ids.noise_reset);
    }

    /// Advance the circuit by `board_cycles` of CPU-clock time.
    pub fn tick(&mut self, board_cycles: u64) {
        self.circuit.tick(board_cycles);
    }

    /// Drain produced mono `i16` samples. Returns the number written.
    pub fn fill_audio(&mut self, out: &mut [i16]) -> usize {
        self.circuit.fill_audio(out)
    }

    /// Output sample rate in Hz.
    pub fn sample_rate(&self) -> u32 {
        self.circuit.sample_rate()
    }

    /// The built circuit, for tooling that reads individual stages.
    ///
    /// Exposed so a comparison run can render one stage on its own. Seven voices
    /// sum into one node here, and thrust alone is five stages deep, so an
    /// output that disagrees says nothing about which stage is at fault.
    pub fn circuit(&self) -> &DiscreteCircuit {
        &self.circuit
    }

    pub fn reset(&mut self) {
        self.circuit.reset();
        self.explosion_reg = 0;
        self.thump_reg = 0;
        self.audio_latch = 0;
    }
}

impl Default for AsteroidsDiscreteSound {
    fn default() -> Self {
        Self::new()
    }
}

impl Saveable for AsteroidsDiscreteSound {
    fn save_state(&self, w: &mut StateWriter) {
        w.write_version(1);
        w.write_u8(self.explosion_reg);
        w.write_u8(self.thump_reg);
        w.write_u8(self.audio_latch);
        self.circuit.save_state(w);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        r.read_version(1)?;
        self.explosion_reg = r.read_u8()?;
        self.thump_reg = r.read_u8()?;
        self.audio_latch = r.read_u8()?;
        self.circuit.load_state(r)?;
        Ok(())
    }
}

impl Debuggable for AsteroidsDiscreteSound {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        let bit = |b: u8| ((self.audio_latch >> b) & 1) as u64;
        vec![
            DebugRegister {
                name: "SAUCER",
                value: bit(0),
                width: 8,
            },
            DebugRegister {
                name: "SAUCER_FIRE",
                value: bit(1),
                width: 8,
            },
            DebugRegister {
                name: "SAUCER_SEL",
                value: bit(2),
                width: 8,
            },
            DebugRegister {
                name: "THRUST",
                value: bit(3),
                width: 8,
            },
            DebugRegister {
                name: "SHIP_FIRE",
                value: bit(4),
                width: 8,
            },
            DebugRegister {
                name: "LIFE",
                value: bit(5),
                width: 8,
            },
            DebugRegister {
                name: "THUMP_EN",
                value: (self.thump_reg & 0x10 != 0) as u64,
                width: 8,
            },
            DebugRegister {
                name: "THUMP_DATA",
                value: (self.thump_reg & 0x0F) as u64,
                width: 8,
            },
            DebugRegister {
                name: "EXPLODE_DATA",
                value: ((self.explosion_reg >> 2) & 0x0F) as u64,
                width: 8,
            },
            DebugRegister {
                name: "EXPLODE_PITCH",
                value: explosion_divider(self.explosion_reg) as u64,
                width: 8,
            },
            DebugRegister {
                name: "MIX",
                value: (self.circuit.value(self.ids.mix).clamp(-1.0, 1.0) * 32767.0) as i16 as u16
                    as u64,
                width: 16,
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_frame(s: &mut AsteroidsDiscreteSound) {
        s.tick(TIMING.cycles_per_frame());
    }

    fn rms(s: &mut AsteroidsDiscreteSound) -> f64 {
        let mut buf = vec![0i16; 4096];
        let n = s.fill_audio(&mut buf);
        if n == 0 {
            return 0.0;
        }
        let sum: f64 = buf[..n].iter().map(|&v| (v as f64).powi(2)).sum();
        (sum / n as f64).sqrt()
    }

    /// The thrust noise must be noise. Its register has to reach every state but
    /// the XNOR lock, which for 16 bits is 32767.
    ///
    /// This shipped as a 42-state cycle, a 286 Hz tone, and measured as the
    /// right pitch the whole time because the band-pass after it rings at its
    /// own resonance whatever it is fed. Nothing else in the suite could see it:
    /// the sequence was deterministic, it toggled, it reset correctly, and the
    /// output's centroid was within a couple of Hz of the board's.
    #[test]
    fn the_thrust_noise_register_runs_a_full_cycle() {
        assert_eq!(
            THRUST_NOISE_LFSR.cycle_length(),
            (1 << 15) - 1,
            "thrust noise is running a short cycle, which is a tone and not noise"
        );
    }

    /// The explosion register is the same 16 bits with the same taps, so it must
    /// agree with the thrust register on the recurrence.
    ///
    /// The board has one noise source feeding both paths. This model builds two,
    /// because the explosion re-clocks its register at a divided rate where the
    /// board samples a free-running one, so they cannot yet be the same node.
    /// They can at least be required to run the same polynomial.
    #[test]
    fn the_explosion_noise_runs_the_same_recurrence_as_thrust() {
        // Seed 0 is one step off the cycle rather than on it, so count where the
        // sequence rejoins itself instead of counting states visited.
        let mut lfsr = ExplosionNoise::SEED;
        let mut seen = std::collections::HashMap::new();
        let mut step = 0u64;
        while !seen.contains_key(&lfsr) {
            seen.insert(lfsr, step);
            let fb = !(((lfsr >> 6) ^ (lfsr >> 14)) & 1) & 1;
            lfsr = (lfsr << 1) | fb;
            step += 1;
        }
        assert_eq!(step - seen[&lfsr], THRUST_NOISE_LFSR.cycle_length());
    }

    #[test]
    fn explosion_register_maps_volume_and_pitch() {
        let mut s = AsteroidsDiscreteSound::new();
        // bits 2-5 = 0b1111 (vol 15), bits 6-7 = 0b10 -> divider 3.
        s.write_explosion(0b1011_1100);
        assert_eq!((s.explosion_reg >> 2) & 0x0F, 0x0F);
        assert_eq!(explosion_divider(s.explosion_reg), 3.0);
        // Each pitch-select code maps as MAME documents.
        assert_eq!(explosion_divider(0b0000_0000), 12.0);
        assert_eq!(explosion_divider(0b0100_0000), 6.0);
        assert_eq!(explosion_divider(0b1000_0000), 3.0);
        assert_eq!(explosion_divider(0b1100_0000), 5.0);
    }

    #[test]
    fn thump_register_maps_enable_and_data() {
        let mut s = AsteroidsDiscreteSound::new();
        s.write_thump(0x1A); // enable (bit 4) + data 0x0A
        assert_eq!(s.thump_reg & 0x10, 0x10);
        assert_eq!(s.thump_reg & 0x0F, 0x0A);
    }

    #[test]
    fn audio_latch_bits_map_to_effects() {
        let mut s = AsteroidsDiscreteSound::new();
        for bit in 0..6u8 {
            s.write_audio_latch_bit(bit, true);
        }
        assert_eq!(s.audio_latch & 0x3F, 0x3F);
        let regs = s.debug_registers();
        // All six latch-driven effect registers should now read 1.
        for name in [
            "SAUCER",
            "SAUCER_FIRE",
            "SAUCER_SEL",
            "THRUST",
            "SHIP_FIRE",
            "LIFE",
        ] {
            let r = regs.iter().find(|r| r.name == name).unwrap();
            assert_eq!(r.value, 1, "{name} should be enabled");
        }
        s.write_audio_latch_bit(3, false);
        assert_eq!(s.audio_latch & 0x08, 0);
    }

    #[test]
    fn audio_drains_after_a_frame_when_active() {
        let mut s = AsteroidsDiscreteSound::new();
        // Silent at power-on (all effects off).
        run_frame(&mut s);
        assert!(rms(&mut s) < 1.0, "should be ~silent with no effects");

        // Enable the life tone -> non-silent, and audio drains.
        s.write_audio_latch_bit(5, true);
        let mut buf = vec![0i16; 8192];
        run_frame(&mut s);
        let n = s.fill_audio(&mut buf);
        assert!(n > 0, "expected samples after a frame");
    }

    #[test]
    fn explosion_produces_output() {
        let mut s = AsteroidsDiscreteSound::new();
        s.write_explosion(0b1011_1100); // max volume, divider 3
        run_frame(&mut s);
        assert!(rms(&mut s) > 1.0, "explosion should be audible");
    }

    #[test]
    fn every_effect_is_audible_when_enabled() {
        // Each effect, enabled in isolation, must produce a clearly non-silent
        // signal. Guards against silent paths (e.g. the D7 latch bug) and
        // under-gained filters (the thrust band-pass make-up).
        for effect in [
            "explosion",
            "thrust",
            "thump",
            "saucer",
            "ship_fire",
            "saucer_fire",
            "life",
        ] {
            let mut s = AsteroidsDiscreteSound::new();
            match effect {
                "explosion" => s.write_explosion(0b1011_1100), // max vol, divider 3
                "thrust" => s.write_audio_latch_bit(3, true),
                "thump" => s.write_thump(0x1F), // enable + max DAC
                "saucer" => s.write_audio_latch_bit(0, true),
                "ship_fire" => s.write_audio_latch_bit(4, true),
                "saucer_fire" => s.write_audio_latch_bit(1, true),
                "life" => s.write_audio_latch_bit(5, true),
                _ => unreachable!(),
            }
            for _ in 0..6 {
                s.tick(TIMING.cycles_per_frame());
            }
            let mut buf = vec![0i16; 16384];
            let n = s.fill_audio(&mut buf);
            assert!(n > 0, "{effect} produced no samples");
            // AC RMS (mean removed) so a stuck-DC signal can't false-pass.
            let mean = buf[..n].iter().map(|&v| v as f64).sum::<f64>() / n as f64;
            let ac: f64 = buf[..n].iter().map(|&v| (v as f64 - mean).powi(2)).sum();
            let rms = (ac / n as f64).sqrt();
            let peak = buf[..n]
                .iter()
                .map(|&v| v.unsigned_abs())
                .max()
                .unwrap_or(0);
            assert!(rms > 150.0, "{effect} should be audible, rms={rms:.0}");
            assert!(peak < 32_760, "{effect} should not hard-clip, peak={peak}");
        }
    }

    #[test]
    fn noise_reset_pulses_without_panicking() {
        let mut s = AsteroidsDiscreteSound::new();
        s.write_explosion(0b0011_1100);
        s.pulse_noise_reset();
        run_frame(&mut s);
        // Just exercising the path; output should still drain.
        let mut buf = vec![0i16; 4096];
        assert!(s.fill_audio(&mut buf) > 0);
    }

    #[test]
    fn save_load_round_trip() {
        let mut s1 = AsteroidsDiscreteSound::new();
        s1.write_explosion(0b0110_1100);
        s1.write_thump(0x15);
        s1.write_audio_latch_bit(3, true); // thrust
        s1.write_audio_latch_bit(5, true); // life
        run_frame(&mut s1);
        let mut discard = vec![0i16; 8192];
        while s1.fill_audio(&mut discard) > 0 {}

        let mut w = StateWriter::new();
        s1.save_state(&mut w);
        let data = w.into_vec();

        let mut s2 = AsteroidsDiscreteSound::new();
        let mut r = StateReader::new(&data);
        s2.load_state(&mut r).unwrap();

        assert_eq!(s2.explosion_reg, s1.explosion_reg);
        assert_eq!(s2.thump_reg, s1.thump_reg);
        assert_eq!(s2.audio_latch, s1.audio_latch);
        assert_eq!(s2.circuit.value(s2.ids.mix), s1.circuit.value(s1.ids.mix));

        // Lock-step audio afterward.
        run_frame(&mut s1);
        run_frame(&mut s2);
        let mut a = vec![0i16; 8192];
        let mut b = vec![0i16; 8192];
        let na = s1.fill_audio(&mut a);
        let nb = s2.fill_audio(&mut b);
        assert_eq!(na, nb);
        assert_eq!(a[..na], b[..nb]);
    }
}
