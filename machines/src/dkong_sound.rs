//! Donkey Kong (TKG-04) discrete sound, built on the [`DiscreteCircuit`]
//! framework. The I8035 DAC stream enters the circuit as an external source and
//! is summed with the three discrete analog effects (walk, jump, stomp) inside
//! the circuit — replacing the old "add the effect sample to the resampled DAC
//! as finished PCM" path. This puts the DAC + effects mixing inside the
//! framework so the shared analog stages (mixer, filters, discharge) have a home.
//!
//! Walk and jump are voltage-controlled 555 astables (R1 = 47 kΩ / R2 = 27 kΩ,
//! C = 33 nF walk / 47 nF jump), built on the framework's [`ne555_astable`]
//! primitive driven by a control-voltage node — a fixed voltage for the walk
//! footstep, an exponential pitch-sweep envelope for the jump. Both are
//! one-shots: the latch bit triggers an envelope rather than gating a running
//! oscillator, because the board thumps once per assertion and does not drone
//! while the bit is held. This replaces the old
//! closed-form `vco_freq()` + phase-accumulator squares, so the cap integration
//! (and its ~73 % duty, harmonics) comes from the real 555 model. Stomp stays on
//! its LFSR-noise model — on hardware it is an LS164 noise source + LS161
//! counter, not a 555. The board talks to the device with hardware intent
//! (`write_sound_bit`, `feed_dac`, `set_discharge`).
//!
//! [`ne555_astable`]: phosphor_core::device::DiscreteCircuitBuilder::ne555_astable

use phosphor_core::core::debug::{DebugRegister, Debuggable};
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::device::{
    CustomComponent, DiscreteCircuit, DiscreteCircuitBuilder, ExternalSourceId, FilterMode,
    LogicInputId, NodeId, Output555, OutputGain,
};

/// Output sample rate. The circuit is built board = sim = output = this rate, so
/// `tick(1)` advances exactly one simulation step; the board drives one step per
/// box-filtered DAC sample.
fn sample_rate() -> u64 {
    phosphor_core::audio::host_sample_rate() as u64
}

// Per-effect output low-pass cutoffs (Hz) and gains, calibrated against captured
// hardware references (tools/sound-reference). On the board each effect passes
// through RC integrators and a coupling filter, so the raw squares/noise are
// darkened by the low-passes and balanced against each other (walk subtle, jump
// moderate, stomp prominent). The absolute gains are scaled up relative to the
// reference because Phosphor's DAC plays at full scale while the board's music
// sits ~5x lower (the VR2 volume pot + mixer): matching the effect/music *ratio*
// keeps the effects audible over the music rather than buried under it.
// The DAC (music) is attenuated to leave output headroom for the effects, the
// way the VR2 volume pot + mixer do on hardware. At full scale the music
// consumed the entire range, so any effect loud enough to hear over it clipped
// against the output clamp. With headroom, the effects sit clearly above.
const DAC_GAIN: f64 = 0.55;
// I8035 DAC reconstruction filter: a Sallen-Key low-pass on the board (R = 5.6 kΩ
// ×2, C = 22 nF / 10 nF) gives f ≈ 1916 Hz, Q ≈ 0.74. It rolls off the DAC step
// edges and sample brightness so the music/effects sit warm rather than hashy.
const DAC_LP_HZ: f64 = 1_916.0;
const DAC_LP_Q: f64 = 0.74;
// Walk, fitted against a MAME capture of the effect triggered once
// (`drive_dkong_single.lua` with DK_EFFECT=walk) against `sndcmp capture
// dkong/walk`:
//
//                  original      now      reference
//   AC RMS        -28.33 dB   -38.59 dB   -38.42 dB
//   fundamental      ~600 Hz    344.5 Hz    338.0 Hz
//   decay T20        0.733 s     0.060 s     0.055 s
//   centroid        512.6 Hz    453.7 Hz    403.0 Hz
//
// The corner stays where the schematic-derived model had it. Dropping it to
// 460 Hz to chase the centroid moved that number by 11 Hz, because what
// dominates it is the oscillator's pitch rather than the filter, and a corner
// below the fundamental only attenuates the note.
//
// The centroid is the last gap and it is a *distribution* difference, not a
// pitch one now that the fundamental matches: the reference spreads its energy
// 15/37/46 % across the bottom three bands where this puts 0/0/97 % in one. A
// single gated square through one pole cannot produce that spread — the board's
// footstep has low-frequency content this model has no source for. Structural,
// so recorded rather than tuned; see `…-dkong-walk-not-sustained-4q42`.
const WALK_LP_HZ: f64 = 700.0;
/// Re-fitted after the envelope changed: a decaying footstep carries far less
/// energy than the sustained tone this used to be, so the level had to come up
/// by ~12 dB to match the same reference.
const WALK_GAIN: f64 = 0.573;
// Walk control voltage, derived from the board's CV network rather than fitted.
// Three currents meet at the 555's CV pin and charge a 3.3 µF cap: a fixed one
// through the chip's own 5 kΩ divider, one through 1 kΩ + 10 kΩ gated by the
// walk latch, and one through 12 kΩ from the wobble oscillator. They see about
// 2.25 kΩ, which sets both the settled voltages and the slew rate.
//
/// CV the 555 settles to while the walk latch is asserted.
const WALK_CV: f64 = 3.745;
/// CV it settles to while the latch is released, the gated current removed.
const WALK_CV_RELEASED: f64 = 2.684;
/// Slew network: 3.3 µF against ~2.25 kΩ, so τ ≈ 7.4 ms — short enough to settle
/// between footsteps, long enough that the pitch is still moving while one
/// sounds. Modelling this as an instant switch is what made every earlier
/// version give a steady pitch per step.
const WALK_CV_SLEW_R: f64 = 2_250.0;
const WALK_CV_SLEW_C: f64 = 3.3e-6;
/// Wobble rate. On the board this is a CMOS inverter oscillator (4.3 kΩ / 43 kΩ
/// / 10 µF), which lands near 1 Hz.
const WALK_LFO_HZ: f64 = 1.16;
/// Wobble depth in volts, from the oscillator's swing through its 12 kΩ.
const WALK_CV_SWING: f64 = 0.47;
/// Walk footstep decay. The reference falls 20 dB in 55 ms, so τ ≈ 55 ms / ln(10).
const WALK_TAU: f64 = 0.024;
/// How long one footstep runs before it is cut off — many time constants, so the
/// envelope decays away on its own rather than being truncated.
const WALK_HOLD_S: f64 = 0.4;
/// Output calibration, not a hardware value: it maps this circuit's volts into
/// the finite PCM range alongside the other effects.
///
/// Re-derived when the envelope became a diode-mixed lid. The board only ever
/// exposes a fraction of the square — a 21 ms trigger dips the lid from 5 V to
/// 3.2 V, and the 555 peaks at 3.8 V, so 0.6 V of a 3.8 V swing gets out, about
/// 16 %. The old value was calibrated against a multiply-based envelope that
/// passed the whole square, so it left the corrected circuit ~14 dB quiet.
const JUMP_GAIN: f64 = 1.218;
// Stomp is a low rumble, not a hiss. Fitted against a MAME capture of the
// effect triggered ONCE (`tools/sound-reference/drive_dkong_single.lua` with
// DK_EFFECT=stomp) against `sndcmp capture dkong/stomp`:
//
//                  original    reference    now
//   AC RMS        -21.72 dB    -23.89 dB   -23.87 dB
//   centroid       250.1 Hz     124.9 Hz    126.7 Hz
//   decay T20       0.090 s      0.545 s     0.374 s
//
// The original 340 Hz corner put most of the energy above the reference's, and
// a 50 ms envelope truncated at 250 ms cut the tail off before the reference
// had finished falling 20 dB.
//
// The gain was first fitted against a repeated-pulse reference window holding
// more than one stomp, which made it read ~3 dB loud and pulled it down to 5.0.
// Measured against a single trigger it is 7.0, close to where it started — a
// reminder that a level fitted against overlapping decays is not a level.
//
// The remaining tail gap is NOT a component value to keep tuning. The reference
// rolls off at 141 Hz where a 175 Hz one-pole rolls off at 215 Hz, and a single
// pole cannot be both as dark as the reference and as loud. The reference falls
// faster than one pole allows, which points at a second filter stage on the
// board. See `…-discrete-sound-fidelity-l5r3.7`.
const STOMP_LP_HZ: f64 = 175.0;
const STOMP_GAIN: f64 = 7.0;
/// Stomp envelope decay, fitted (see above). Longer than T20/ln(10) would
/// suggest because the output low-pass smears the envelope's start.
const STOMP_TAU: f64 = 0.39;
/// How long the burst runs before it is cut off. Several time constants, so the
/// envelope decays away rather than being truncated mid-fall as it was at 250 ms.
const STOMP_HOLD_S: f64 = 1.2;

// Shared 555 astable values for the walk/jump VCOs: R1 charges through 47 kΩ,
// R2 discharges through 27 kΩ, Vcc = 5 V. The control voltage on pin 5 sets
// threshold = CV, trigger = CV/2, so a higher CV is a slower charge and a lower
// frequency.
const VCC: f64 = 5.0;
const R1: f64 = 47_000.0;
const R2: f64 = 27_000.0;
const WALK_C: f64 = 33e-9;
const JUMP_C: f64 = 47e-9;
/// 555 output-high level. The absolute value is folded into the per-effect
/// gains; 1.0 keeps the post-DC-block square at ±~0.5 before calibration.
const OUT_HIGH: f64 = 1.0;
/// DC-blocking high-pass matching the walk path's coupling network (11.2 kΩ,
/// 4.7 µF, ≈3 Hz). The 555 square's duty (and so its DC offset) shifts with CV,
/// so AC-couple before filtering rather than subtracting a fixed mean.
const AC_R: f64 = 11_200.0;
const AC_C: f64 = 4.7e-6;
// Jump's control-voltage network, derived like walk's: currents through
// 10 kΩ + 10 kΩ (latch-gated) and the 555's own 5 kΩ divider meet at a 10 µF cap
// behind 1.2 kΩ, seeing about 2.3 kΩ.
//
/// CV the 555 settles to while the jump latch is asserted.
const JUMP_CV_ASSERTED: f64 = 3.51;
/// CV it falls back to once released. Lower CV is a higher frequency, so the
/// decay toward this is the jump's upward sweep.
const JUMP_CV_RELEASED: f64 = 2.71;
/// Slew network: 10 µF against ~2.3 kΩ, τ ≈ 22 ms.
const JUMP_CV_SLEW_R: f64 = 2_300.0;
const JUMP_CV_SLEW_C: f64 = 10e-6;
/// Jump's wobble rate: 18 kΩ with 3.3 µF, near 8 Hz — fast enough that a
/// half-second jump warbles several times as it sweeps.
const JUMP_LFO_HZ: f64 = 8.4;
/// Wobble depth in volts, from the oscillator's swing through its own 10 kΩ into
/// the ~2.3 kΩ the CV currents see.
const JUMP_CV_SWING: f64 = 0.575;
/// 1N5553 forward drop through the output diodes, expressed in the normalized
/// units this circuit works in rather than in volts: 0.4 V against the 555's
/// ~3.8 V high, which `OUT_HIGH` stands for.
const DIODE_DROP: f64 = 0.4 / 3.8;
/// Output integrator: 150 Ω charging 1 µF, so a corner near 1.06 kHz.
///
/// The 4.7 kΩ ∥ (2 kΩ + 5.1 kΩ) in the board's integrator is the *discharge*
/// path, not the corner — folding it in gave 2.8 kΩ and a 57 Hz corner, which
/// removed most of the note and left the jump 13 dB quiet and far too dark.
const JUMP_OUT_R: f64 = 150.0;
const JUMP_OUT_C: f64 = 1e-6;
/// Output divider, 5.1 kΩ of the 2 kΩ + 5.1 kΩ that follows the integrator.
const JUMP_DIVIDER: f64 = 5.1 / 7.1;
/// How far the lid rests above the 555's peak, as a ratio. The board's lid
/// charges to the 5 V supply while the 555 tops out at Vcc − 1.2 ≈ 3.8 V.
const JUMP_LID_HEADROOM: f64 = 5.0 / 3.8;
/// Conditioned trigger width. The latch edge is differentiated by 1 µF into
/// 10 kΩ (τ = 10 ms) and compared against 0.6 V, so the pulse lasts
/// 10 ms · ln(5/0.6) ≈ 21 ms whatever the latch does.
const JUMP_TRIG_S: f64 = 0.021;
/// How fast the jump's lid is pulled down when the latch asserts: 10 kΩ into
/// 4.7 µF. The fast half of the asymmetry — a jump snaps open.
const JUMP_ENV_DIP_S: f64 = 0.047;
/// How slowly it recovers once released: 100 kΩ + 10 kΩ into 4.7 µF. The slow
/// half — and closing over the oscillator is what ends the note.
const JUMP_ENV_RECOVER_S: f64 = 0.517;

// ---------------------------------------------------------------------------
// Effect components (custom escape hatch)
// ---------------------------------------------------------------------------

/// Fixed-width pulse on the rising edge of its input, regardless of how long the
/// input stays asserted.
///
/// Models the board's trigger conditioner: the latch edge is differentiated by a
/// 1 µF cap into 10 kΩ, and a comparator passes the resulting spike while it
/// stays above 0.6 V. Since the spike decays with a 10 ms time constant from
/// roughly the supply, that is about 21 ms — and crucially it does not depend on
/// the latch pulse width, so a game that holds the line longer does not get a
/// longer envelope.
///
/// Without this the envelope followed the latch directly, which tied the note's
/// length to a pulse width the circuit deliberately discards.
struct DkongEdgePulse {
    width: f64,
    timer: f64,
    active: bool,
    last_en: bool,
}

impl CustomComponent for DkongEdgePulse {
    fn reset(&mut self) {
        self.timer = 0.0;
        self.active = false;
        self.last_en = false;
    }
    fn step(&mut self, inputs: &[f64], dt: f64) -> f64 {
        let en = inputs[0] > 0.5;
        if en && !self.last_en {
            self.active = true;
            self.timer = 0.0;
        }
        self.last_en = en;
        if !self.active {
            return 0.0;
        }
        self.timer += dt;
        if self.timer > self.width {
            self.active = false;
            return 0.0;
        }
        1.0
    }
    fn save_state(&self, w: &mut StateWriter) {
        w.write_f64_le(self.timer);
        w.write_bool(self.active);
        w.write_bool(self.last_en);
    }
    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.timer = r.read_f64_le()?;
        self.active = r.read_bool()?;
        self.last_en = r.read_bool()?;
        Ok(())
    }
}

/// One-shot exponential decay, triggered on the rising edge of its enable and
/// cut off after `hold`.
///
/// Shared by walk and jump, which differ only in their constants: walk is a
/// 24 ms footstep driving amplitude alone, jump a 360 ms decay driving both the
/// 555 control voltage (`1 + 3·env`, so the pitch sweeps up as it decays) and
/// the amplitude. Envelope shaping only — the oscillator is the
/// [`ne555_astable`](phosphor_core::device::DiscreteCircuitBuilder::ne555_astable)
/// node. Input: `[enable]`.
struct DkongOneShotEnv {
    /// Amplitude decay time constant, seconds.
    tau: f64,
    /// How long the envelope runs before it is cut off, seconds.
    hold: f64,
    /// Retrigger on *either* edge rather than only the rising one.
    ///
    /// Walk needs this; jump does not. The game pulses the walk line for 3
    /// frames every 12 while Mario moves, and the board sounds on both edges of
    /// that pulse at two different pitches — so each step is a note at 0 ms and
    /// another 50 ms later, repeating every 200 ms. That is the alternating
    /// two-tone footstep. Triggering on the rising edge alone gives a single
    /// repeated tone, which is audibly wrong however well its pitch is fitted.
    both_edges: bool,
    /// Amplitude of a falling-edge trigger relative to a rising one. The two
    /// notes are not a matched pair. Only meaningful with `both_edges`.
    falling_scale: f64,
    /// Which edge started the envelope currently running.
    on_falling: bool,
    active: bool,
    timer: f64,
    last_en: bool,
}

impl CustomComponent for DkongOneShotEnv {
    fn reset(&mut self) {
        self.active = false;
        self.timer = 0.0;
        self.last_en = false;
    }
    fn step(&mut self, inputs: &[f64], dt: f64) -> f64 {
        let en = inputs[0] > 0.5;
        let triggered = if self.both_edges {
            en != self.last_en
        } else {
            en && !self.last_en
        };
        if triggered {
            self.active = true;
            self.timer = 0.0;
            self.on_falling = !en;
        }
        self.last_en = en;
        if !self.active {
            return 0.0;
        }
        self.timer += dt;
        if self.timer > self.hold {
            self.active = false;
            return 0.0;
        }
        let scale = if self.on_falling {
            self.falling_scale
        } else {
            1.0
        };
        scale * (-self.timer / self.tau).exp()
    }
    fn save_state(&self, w: &mut StateWriter) {
        w.write_bool(self.active);
        w.write_f64_le(self.timer);
        w.write_bool(self.last_en);
    }
    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.active = r.read_bool()?;
        self.timer = r.read_f64_le()?;
        self.last_en = r.read_bool()?;
        Ok(())
    }
}

/// Stomp: a 24-bit LFSR noise burst (4 kHz clock) with a [`STOMP_TAU`] amplitude
/// decay, triggered on the rising edge of the enable. Input: `[stomp_en]`.
struct DkongStomp {
    active: bool,
    timer: f64,
    lfsr: u32,
    lfsr_clock: f64,
    last_en: bool,
}

impl DkongStomp {
    const SEED: u32 = 0x1A_CFFC;
}

impl CustomComponent for DkongStomp {
    fn reset(&mut self) {
        self.active = false;
        self.timer = 0.0;
        self.lfsr = Self::SEED;
        self.lfsr_clock = 0.0;
        self.last_en = false;
    }
    fn step(&mut self, inputs: &[f64], dt: f64) -> f64 {
        let en = inputs[0] > 0.5;
        if en && !self.last_en {
            self.active = true;
            self.timer = 0.0;
        }
        self.last_en = en;
        if !self.active {
            return 0.0;
        }
        self.timer += dt;
        if self.timer > STOMP_HOLD_S {
            self.active = false;
            return 0.0;
        }
        self.lfsr_clock += 4000.0 * dt;
        while self.lfsr_clock >= 1.0 {
            self.lfsr_clock -= 1.0;
            let bit = ((self.lfsr >> 10) ^ (self.lfsr >> 23)) & 1;
            self.lfsr = (self.lfsr >> 1) | (bit << 23);
        }
        let noise = if self.lfsr & 1 != 0 { 1.0 } else { -1.0 };
        let amp = (-self.timer / STOMP_TAU).exp();
        noise * amp * 0.12
    }
    fn save_state(&self, w: &mut StateWriter) {
        w.write_bool(self.active);
        w.write_f64_le(self.timer);
        w.write_u32_le(self.lfsr);
        w.write_f64_le(self.lfsr_clock);
        w.write_bool(self.last_en);
    }
    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.active = r.read_bool()?;
        self.timer = r.read_f64_le()?;
        self.lfsr = r.read_u32_le()?;
        self.lfsr_clock = r.read_f64_le()?;
        self.last_en = r.read_bool()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Circuit + wrapper
// ---------------------------------------------------------------------------

struct DkongInputs {
    dac: ExternalSourceId,
    walk_en: LogicInputId,
    jump_en: LogicInputId,
    stomp_en: LogicInputId,
    discharge: LogicInputId,
    mix: NodeId,
}

fn build_circuit() -> (DiscreteCircuit, DkongInputs) {
    let mut b = DiscreteCircuitBuilder::new(sample_rate(), sample_rate());

    let dac = b.external_source("DAC"); // normalized I8035 DAC stream
    let walk_en = b.logic_input("WALK_EN");
    let jump_en = b.logic_input("JUMP_EN");
    let stomp_en = b.logic_input("STOMP_EN");
    // Discharge control line — wired for the board-level mute/discharge path,
    // currently inert (the TKG-04 model does not drive it yet).
    let discharge = b.logic_input("DISCHARGE");

    // Walk: a fixed 555 control voltage, AC-coupled to drop the duty-dependent
    // DC, shaped by a one-shot envelope, darkened by the output low-pass.
    //
    // This used to add a 1 Hz ±0.65 V triangle LFO to the control voltage. That
    // made sense while walk was a sustained drone — it gave a continuous tone
    // some movement. It stopped making sense the moment walk became a 24 ms
    // footstep: a burst that short samples the LFO at whatever phase it happens
    // to occupy when the step fires, so every footstep came out at a different
    // pitch depending on *when* it was triggered. Measuring the same voice
    // across three captures gave 551, 377 and 604 Hz, which read as an unstable
    // estimator and was really an unstable oscillator.
    //
    // A fixed CV also lets the pitch be solved for rather than guessed. With
    // R1 = 47 kΩ, R2 = 27 kΩ, C = 33 nF and Vcc = 5 V the 555 charges from CV/2
    // to CV through R1+R2 and discharges through R2, so
    //     T = (R1+R2)·C·ln((Vcc − CV/2)/(Vcc − CV)) + R2·C·ln2
    // and the reference's 338 Hz wants CV ≈ 3.35 V.
    // A slow oscillator wobbles the 555's control voltage, so successive
    // footsteps come out at different pitches.
    //
    // This is where the walk's alternating two-tone character comes from, and
    // it is easy to mistake for a bug: a footstep lasts tens of milliseconds
    // and samples the wobble wherever it happens to be, so measuring one step
    // in isolation gives a pitch that appears to drift run to run. It is not
    // drift — the board really does vary the pitch per step, and removing the
    // wobble to "stabilise" it produces a single repeated tone that is audibly
    // wrong however precisely its frequency is fitted.
    //
    // On the board this is a CMOS inverter oscillator (4.3 kΩ / 43 kΩ / 10 µF),
    // which puts it near 1 Hz — slow enough that consecutive steps 200 ms apart
    // land on meaningfully different parts of the cycle.
    // The control voltage is a capacitor, not a switch.
    //
    // Three currents feed the 555's CV pin through a 3.3 µF cap: a fixed one
    // from the chip's own divider, one gated by the walk latch, and one from the
    // wobble oscillator. The cap makes the CV *slew* between its two latched
    // levels with a ~7.4 ms time constant rather than stepping — and since a
    // footstep only lasts a few tens of milliseconds, the pitch is still moving
    // while the note sounds. Each step is a small downward sweep, not a tone.
    //
    // That slew is the piece every earlier attempt was missing. Modelling the CV
    // as instantaneous gives a steady pitch per step, which measures plausibly
    // and sounds wrong; a hold-based capture cannot reveal it because after two
    // seconds the cap has long since settled.
    let walk_lfo = b.triangle("WALK_LFO", WALK_LFO_HZ);
    let walk_lfo_s = b.gain("WALK_LFO_S", walk_lfo, WALK_CV_SWING);
    let walk_cv_base = b.constant("WALK_CV_BASE", WALK_CV_RELEASED);
    let walk_cv_gate = b.gain("WALK_CV_GATE", walk_en, WALK_CV - WALK_CV_RELEASED);
    let walk_cv_raw = b.add("WALK_CV_RAW", &[walk_cv_base, walk_cv_gate, walk_lfo_s]);
    let walk_cv = b.rc_low_pass("WALK_CV", walk_cv_raw, WALK_CV_SLEW_R, WALK_CV_SLEW_C);
    let walk_555 = b.ne555_astable(
        "WALK_555",
        Some(walk_cv),
        R1,
        R2,
        WALK_C,
        VCC,
        OUT_HIGH,
        Output555::Square,
    );
    let walk_ac = b.rc_high_pass("WALK_AC", walk_555, AC_R, AC_C);
    // Walk is a footstep — one thump per assertion, not a tone that runs for as
    // long as the latch bit is held. Measured against a reference that holds the
    // bit for two full seconds, the board sounds for about half of one and
    // decays 20 dB in 55 ms; gating a free-running oscillator by the level
    // instead gave a 733 ms drone 10 dB too loud. So the enable triggers a
    // one-shot envelope, exactly as jump's does.
    let walk_env = b.custom(
        "WALK_ENV",
        vec![walk_en.into()],
        Box::new(DkongOneShotEnv {
            tau: WALK_TAU,
            both_edges: false,
            falling_scale: 1.0,
            on_falling: false,
            hold: WALK_HOLD_S,
            active: false,
            timer: 0.0,
            last_en: false,
        }),
    );
    let walk_gated = b.multiply("WALK_GATED", walk_ac, walk_env);
    let walk_lp = b.second_order(
        "WALK_LP",
        walk_gated,
        FilterMode::LowPass,
        WALK_LP_HZ,
        0.707,
    );
    let walk = b.gain("WALK_OUT", walk_lp, WALK_GAIN);

    // Jump's envelope is an asymmetric RC following the latch, not a one-shot.
    //
    // On the board a 4.7 µF cap is pulled toward ground through 10 kΩ while the
    // latch is asserted and recovers toward the supply through 110 kΩ when it is
    // released — 47 ms down, 517 ms back. The asymmetry is the whole character:
    // a jump snaps open and closes slowly.
    //
    // A single-time-constant one-shot cannot express that. It dipped instantly
    // and recovered at one rate, which made the decay about twice as long as the
    // board's while losing the fast attack entirely. Following the latch through
    // an asymmetric RC needs no one-shot at all, because the latch pulse *is*
    // the trigger — which is also what the board does.
    // The envelope is driven by the conditioned trigger, not the latch: a fixed
    // ~21 ms pulse from the edge, whatever the game's pulse width.
    let jump_trig = b.custom(
        "JUMP_TRIG",
        vec![jump_en.into()],
        Box::new(DkongEdgePulse {
            width: JUMP_TRIG_S,
            timer: 0.0,
            active: false,
            last_en: false,
        }),
    );

    // Target rests *above* the square's peak, not level with it.
    //
    // On the board the lid charges toward the 5 V supply while the 555 tops out
    // at Vcc − 1.2 ≈ 3.8 V, so the lid has headroom to close fully over the
    // oscillator — and the note ends at the finite moment it crosses the peak.
    // Resting exactly at the peak instead makes the lid approach it
    // asymptotically, so the note never quite stops and the decay measures
    // several times too long however the time constants are set.
    let jump_lid_rest = b.constant("JUMP_LID_REST", OUT_HIGH * JUMP_LID_HEADROOM);
    let jump_lid_dip = b.gain("JUMP_LID_DIP", jump_trig, -OUT_HIGH * JUMP_LID_HEADROOM);
    let jump_lid_target = b.add("JUMP_LID_TARGET", &[jump_lid_rest, jump_lid_dip]);
    let jump_lid = b.rc_envelope(
        "JUMP_LID",
        jump_lid_target,
        JUMP_ENV_RECOVER_S,
        JUMP_ENV_DIP_S,
    );
    // The pitch sweep is the control-voltage capacitor discharging, not the
    // amplitude envelope in disguise.
    //
    // Jump's CV network is the same shape as walk's — a latch-gated current and
    // a fixed one meeting at a cap — but with 10 µF instead of 3.3 µF, so it
    // slews with τ ≈ 22 ms between 3.51 V asserted and 2.71 V released. A lower
    // control voltage is a faster charge and so a higher frequency, which is why
    // a jump sweeps *upward* as the cap discharges after the trigger.
    //
    // This used to drive the CV straight from the amplitude envelope
    // (`1 + 3·env`), which produces a sweep of roughly the right direction from
    // the wrong mechanism: the sweep and the amplitude decay were forced to
    // share one time constant, when the board gives them two (22 ms and ~0.5 s).
    //
    // Jump has its own wobble oscillator, and it is audible.
    //
    // 18 kΩ with 3.3 µF puts it near 8 Hz — several cycles across a half-second
    // jump, so the note warbles as it sweeps. It enters the control voltage
    // through its own 10 kΩ, alongside the latch-gated current, so the wobble
    // and the sweep add rather than one modulating the other.
    //
    // I first read this circuit's 3.3 MΩ as the timing resistor and concluded
    // the oscillator ran at 0.05 Hz — effectively a DC offset — and left it out.
    // That resistor is the bias pull; the first of the pair sets the rate.
    let jump_lfo = b.triangle("JUMP_LFO", JUMP_LFO_HZ);
    let jump_lfo_s = b.gain("JUMP_LFO_S", jump_lfo, JUMP_CV_SWING);
    let jump_cv_base = b.constant("JUMP_CV_BASE", JUMP_CV_RELEASED);
    let jump_cv_gate = b.gain("JUMP_CV_GATE", jump_en, JUMP_CV_ASSERTED - JUMP_CV_RELEASED);
    let jump_cv_raw = b.add("JUMP_CV_RAW", &[jump_cv_base, jump_cv_gate, jump_lfo_s]);
    let jump_cv = b.rc_low_pass("JUMP_CV", jump_cv_raw, JUMP_CV_SLEW_R, JUMP_CV_SLEW_C);
    let jump_555 = b.ne555_astable(
        "JUMP_555",
        Some(jump_cv),
        R1,
        R2,
        JUMP_C,
        VCC,
        OUT_HIGH,
        Output555::Square,
    );
    // The envelope is DIODE-MIXED with the oscillator, not multiplied by it.
    //
    // Two diodes meet at the output node, so what reaches it is whichever of the
    // envelope and the 555 square is *higher*, less a forward drop. That is a
    // completely different combining rule from multiplication, and it is what
    // gives a jump its shape: while the envelope is high it holds the node above
    // the square's peaks, so the start is a swell with no tone in it at all; as
    // the envelope decays the square begins to poke through the top, so the note
    // emerges and grows. The envelope acts as a falling floor that clips the
    // oscillator from below, which also means the audible duty cycle changes
    // over the effect's life.
    //
    // Multiplying instead scales the oscillator by the envelope — tone from the
    // first instant, uniform duty, decaying amplitude. Recognisably a jump, and
    // wrong in a way no amount of retuning the envelope could fix.
    // The envelope is a LID that rests high and is pulled down by the trigger,
    // not a floor that rises.
    //
    // The 555 free-runs whether or not anything is jumping, so at rest something
    // has to keep it off the output. That something is the envelope: it sits
    // above the square's peaks, and since the diodes pass whichever input is
    // higher, the node holds a constant DC that the coupling capacitor then
    // removes — silence. Triggering pulls the lid down, the square rises above
    // it, and the note is heard; as the lid recovers it closes over the peaks
    // again and the note fades.
    //
    // Getting this backwards is audible rather than subtle: a lid that rests low
    // lets the free-running oscillator through permanently, so the board hums
    // continuously and the jump itself does nothing.
    //
    // Both inputs must share a scale for a max() to mean anything. The 555 emits
    // `OUT_HIGH` rather than volts here (its absolute level is folded into the
    // per-effect gains), so the lid is expressed in the same units.
    let jump_mixed = b.diode_mixer("JUMP_MIX", &[jump_lid, jump_555], DIODE_DROP);

    // Output stage: an RC integrator into the divider that feeds the mixer,
    // rather than a low-pass at a guessed corner. 150 Ω charging 1 µF against
    // 4.7 kΩ ∥ (2 kΩ + 5.1 kΩ) is a far gentler corner than the 1400 Hz this
    // used to assume, which is why the old model read twice as bright as the
    // reference.
    // No coupling capacitor here. Walk's chain has one; jump's does not — it
    // runs diode mixer straight into the integrator and on to the board mixer,
    // where the shared output coupling removes the DC.
    //
    // Adding one costs more than it looks. The lid resting high means the node
    // sits at a large steady DC, and the trigger steps it down sharply; a
    // high-pass turns that step into a big low-frequency transient that
    // dominates the whole effect. It pulled the spectral centroid to 48 Hz
    // against the reference's 312, and no amount of level correction fixes a
    // spectrum made mostly of a thump.
    let jump_int = b.rc_low_pass("JUMP_INT", jump_mixed, JUMP_OUT_R, JUMP_OUT_C);
    let jump = b.gain("JUMP_OUT", jump_int, JUMP_GAIN * JUMP_DIVIDER);

    let stomp_raw = b.custom(
        "STOMP",
        vec![stomp_en.into()],
        Box::new(DkongStomp {
            active: false,
            timer: 0.0,
            lfsr: DkongStomp::SEED,
            lfsr_clock: 0.0,
            last_en: false,
        }),
    );
    let stomp_lp = b.second_order(
        "STOMP_LP",
        stomp_raw,
        FilterMode::LowPass,
        STOMP_LP_HZ,
        0.707,
    );
    let stomp = b.gain("STOMP_OUT", stomp_lp, STOMP_GAIN);

    // DAC reconstruction: a Sallen-Key low-pass darkens the raw DAC steps before
    // the mix, matching the board's filter so the sampled music isn't hashy.
    let dac_lp = b.second_order("DAC_LP", dac, FilterMode::LowPass, DAC_LP_HZ, DAC_LP_Q);

    // Op-amp summing mixer: the filtered DAC plus the three filtered effects.
    let mix = b.add("MIX", &[dac_lp, walk, jump, stomp]);

    // Output coupling capacitor.
    //
    // Walk and jump are AC-coupled individually above, but the I8035 DAC is
    // unipolar — its codes span 0..Vref, so its rest level is a pedestal, not
    // zero — and nothing was removing that from the summed output. It reached
    // the speaker as a slow drift that carried 94 % of the board's measured
    // energy below 150 Hz, with a spectral centroid of 32 Hz: content no
    // cabinet speaker reproduces, swamping the walk/jump/stomp voices this
    // circuit exists to model.
    //
    // The corner matches the per-effect couplings already here (11.2 kΩ /
    // 4.7 µF ≈ 3 Hz), which is the same part value the board uses on its
    // output stage.
    let out_ac = b.rc_high_pass("OUT_AC", mix, AC_R, AC_C);
    b.output(out_ac, OutputGain::unity());

    let circuit = b.build();
    (
        circuit,
        DkongInputs {
            dac,
            walk_en,
            jump_en,
            stomp_en,
            discharge,
            mix,
        },
    )
}

/// Donkey Kong discrete sound device: the DAC stream summed with the walk, jump,
/// and stomp effects inside a [`DiscreteCircuit`].
pub struct DkongDiscreteSound {
    circuit: DiscreteCircuit,
    ids: DkongInputs,
    /// 74LS259 sound control latch bits 0-2 (walk/jump/stomp).
    latch: u8,
    discharge: bool,
}

impl DkongDiscreteSound {
    pub fn new() -> Self {
        let (circuit, ids) = build_circuit();
        Self {
            circuit,
            ids,
            latch: 0,
            discharge: false,
        }
    }

    /// Set a sound-control latch bit (0 = walk, 1 = jump, 2 = stomp).
    pub fn write_sound_bit(&mut self, bit: u8, value: bool) {
        if value {
            self.latch |= 1 << bit;
        } else {
            self.latch &= !(1 << bit);
        }
        match bit {
            0 => self.circuit.set_logic(self.ids.walk_en, value),
            1 => self.circuit.set_logic(self.ids.jump_en, value),
            2 => self.circuit.set_logic(self.ids.stomp_en, value),
            _ => {}
        }
    }

    /// Feed one box-filtered I8035 DAC sample (signed i16) and advance the
    /// circuit by one step, producing one output sample.
    pub fn feed_dac(&mut self, sample: i16) {
        self.circuit
            .set_external(self.ids.dac, sample as f64 / 32767.0 * DAC_GAIN);
        self.circuit.tick(1);
    }

    /// Set the discharge/mute control line.
    pub fn set_discharge(&mut self, value: bool) {
        self.discharge = value;
        self.circuit.set_logic(self.ids.discharge, value);
    }

    /// Drain produced mono `i16` samples. Returns the number written.
    pub fn fill_audio(&mut self, out: &mut [i16]) -> usize {
        self.circuit.fill_audio(out)
    }

    /// The built circuit, for tooling that reads individual stages.
    ///
    /// Exposed so a comparison run can render one voice on its own — a mixed
    /// sum cannot say which of walk, jump or stomp is wrong, and they overlap in
    /// frequency, so no analysis of the sum recovers it.
    pub fn circuit(&self) -> &DiscreteCircuit {
        &self.circuit
    }

    pub fn reset(&mut self) {
        self.circuit.reset();
        self.latch = 0;
        self.discharge = false;
    }
}

impl Default for DkongDiscreteSound {
    fn default() -> Self {
        Self::new()
    }
}

impl phosphor_core::device::Device for DkongDiscreteSound {
    fn name(&self) -> &'static str {
        "DK Discrete"
    }
    fn reset(&mut self) {
        self.reset();
    }
}

impl Saveable for DkongDiscreteSound {
    fn save_state(&self, w: &mut StateWriter) {
        w.write_version(1);
        w.write_u8(self.latch);
        w.write_bool(self.discharge);
        self.circuit.save_state(w);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        r.read_version(1)?;
        self.latch = r.read_u8()?;
        self.discharge = r.read_bool()?;
        self.circuit.load_state(r)?;
        Ok(())
    }
}

impl Debuggable for DkongDiscreteSound {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        let bit = |b: u8| ((self.latch >> b) & 1) as u64;
        let sample = |v: f64| (v.clamp(-1.0, 1.0) * 32767.0) as i16 as u16 as u64;
        vec![
            DebugRegister {
                name: "LATCH",
                value: self.latch as u64,
                width: 8,
            },
            DebugRegister {
                name: "WALK",
                value: bit(0),
                width: 8,
            },
            DebugRegister {
                name: "JUMP",
                value: bit(1),
                width: 8,
            },
            DebugRegister {
                name: "STOMP",
                value: bit(2),
                width: 8,
            },
            DebugRegister {
                name: "DAC",
                value: sample(self.circuit.value(self.ids.dac)),
                width: 16,
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

    /// Run `n` output samples with a fixed DAC value.
    fn run(s: &mut DkongDiscreteSound, dac: i16, n: usize) {
        for _ in 0..n {
            s.feed_dac(dac);
        }
    }

    fn drain_rms(s: &mut DkongDiscreteSound) -> f64 {
        let mut buf = vec![0i16; 8192];
        let n = s.fill_audio(&mut buf);
        if n == 0 {
            return 0.0;
        }
        let sum: f64 = buf[..n].iter().map(|&v| (v as f64).powi(2)).sum();
        (sum / n as f64).sqrt()
    }

    /// The DAC reaches the output, and its *DC* does not.
    ///
    /// This used to assert that a held DAC value settled to a constant at the
    /// output, which is exactly the defect the output coupling capacitor now
    /// removes: the I8035 DAC is unipolar, so a held code is a pedestal, and
    /// letting it through put 94 % of the board's energy below 150 Hz and
    /// swamped the walk/jump/stomp voices. A coupling capacitor passes the step
    /// and then decays to zero, which is what a real one does and what this now
    /// checks.
    #[test]
    fn the_dac_reaches_the_output_but_its_dc_does_not() {
        let mut s = DkongDiscreteSound::new();
        // The coupling network is 11.2 kΩ / 4.7 µF, so τ ≈ 53 ms. Hold the code
        // for a second — about twenty time constants — so the decay is fully
        // resolved rather than caught mid-slope.
        run(&mut s, 10_000, 44_100);
        let mut buf = vec![0i16; 65_536];
        let n = s.fill_audio(&mut buf);
        assert!(n > 1000, "expected about a second of audio, got {n}");

        // The step itself gets through: the output moves when the DAC does.
        let peak = buf[..n].iter().map(|v| v.abs()).max().unwrap();
        assert!(
            peak > (10_000.0 * DAC_GAIN * 0.5) as i16,
            "the DAC step should reach the output, peak was {peak}"
        );

        // And the held level decays away rather than sitting there as a pedestal.
        let tail_slice = &buf[n - 200..n];
        let tail = (tail_slice.iter().map(|&v| (v as f64).powi(2)).sum::<f64>()
            / tail_slice.len() as f64)
            .sqrt();
        assert!(
            tail < peak as f64 * 0.05,
            "a held DAC code must not survive as DC: peak {peak}, tail RMS {tail:.1}"
        );
    }

    #[test]
    fn effects_add_on_top_of_dac() {
        // The stomp is one of the louder effects (walk is intentionally near
        // inaudible, matching real DK), so use it to check effects reach the mix.
        let mut s = DkongDiscreteSound::new();
        s.write_sound_bit(2, true); // stomp
        run(&mut s, 0, 200);
        assert!(
            drain_rms(&mut s) > 100.0,
            "stomp should be audible over silence"
        );
    }

    #[test]
    fn jump_and_stomp_are_edge_triggered_one_shots() {
        for bit in [1u8, 2u8] {
            let mut s = DkongDiscreteSound::new();
            s.write_sound_bit(bit, true); // rising edge triggers
            run(&mut s, 0, 1000);
            assert!(drain_rms(&mut s) > 50.0, "bit {bit} one-shot should sound");
        }
    }

    #[test]
    fn debug_registers_reflect_latch() {
        let mut s = DkongDiscreteSound::new();
        s.write_sound_bit(1, true);
        let regs = s.debug_registers();
        let get = |n: &str| regs.iter().find(|r| r.name == n).unwrap().value;
        assert_eq!(get("LATCH"), 0x02);
        assert_eq!(get("JUMP"), 1);
        assert_eq!(get("WALK"), 0);
    }

    #[test]
    fn save_load_round_trip() {
        let mut s1 = DkongDiscreteSound::new();
        s1.write_sound_bit(0, true);
        s1.write_sound_bit(1, true);
        run(&mut s1, 5_000, 300);
        let mut discard = vec![0i16; 8192];
        while s1.fill_audio(&mut discard) > 0 {}

        let mut w = StateWriter::new();
        s1.save_state(&mut w);
        let data = w.into_vec();

        let mut s2 = DkongDiscreteSound::new();
        let mut r = StateReader::new(&data);
        s2.load_state(&mut r).unwrap();
        assert_eq!(s2.latch, s1.latch);
        assert_eq!(s2.circuit.value(s2.ids.mix), s1.circuit.value(s1.ids.mix));

        run(&mut s1, 5_000, 100);
        run(&mut s2, 5_000, 100);
        let mut a = vec![0i16; 8192];
        let mut b = vec![0i16; 8192];
        let na = s1.fill_audio(&mut a);
        let nb = s2.fill_audio(&mut b);
        assert_eq!(na, nb);
        assert_eq!(a[..na], b[..nb]);
    }
}
