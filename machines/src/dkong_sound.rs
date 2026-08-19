//! Donkey Kong (TKG-04) discrete sound, built on the [`DiscreteCircuit`]
//! framework. The I8035 DAC stream enters the circuit as an external source and
//! is summed with the three discrete analog effects (walk, jump, stomp) inside
//! the circuit — replacing the old "add the effect sample to the resampled DAC
//! as finished PCM" path. This puts the DAC + effects mixing inside the
//! framework so the shared analog stages (mixer, filters, discharge) have a home.
//!
//! Walk and jump are voltage-controlled 555 astables (R1 = 47 kΩ / R2 = 27 kΩ,
//! C = 33 nF walk / 47 nF jump), built on the framework's [`ne555_astable`]
//! primitive driven by a control-voltage node, so the cap integration (and its
//! ~73 % duty, and its harmonics) comes from the real 555 model. Stomp's source
//! is instead a shift register clocked at 4 kHz feeding a counter that divides
//! its edges by eight — an LS164 chain into an LS161, which is where its rumble
//! gets its 125 Hz. All three sources free-run; what makes a note is the
//! envelope opening over one. The board talks to the device with hardware
//! intent (`write_sound_bit`, `feed_dac`, `set_discharge`).
//!
//! Jump and stomp share one chain, stage for stage and largely value for value:
//! a fixed-width conditioned trigger, an envelope that is diode-mixed with the
//! source rather than multiplied by it, and an emitter follower rather than a
//! filter on the output, into a divider. Both work in volts throughout, because
//! every one of those stages compares two absolute voltages against each other.
//! Jump adds a slewing control-voltage capacitor with its own wobble oscillator,
//! which is the only part of the two that differs downstream of the source.
//!
//! Downstream of the mix, three stages belong to the board rather than to any
//! voice and apply to the music as well: the summing node's own 100 nF
//! (~295 Hz), and the amplifier's couplings and emitter bypass, which together
//! put the board's low-frequency limit near 34 Hz rather than the 3 Hz a single
//! coupling capacitor suggests.
//!
//! [`ne555_astable`]: phosphor_core::device::DiscreteCircuitBuilder::ne555_astable

use phosphor_core::core::debug::{DebugRegister, Debuggable};
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::device::{
    CustomComponent, DiscreteCircuit, DiscreteCircuitBuilder, ExternalSourceId, FilterMode,
    LfsrSpec, LogicInputId, NodeId, Output555, OutputGain,
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
//   AC RMS        -28.33 dB   -38.75 dB   -38.42 dB
//   decay T20        0.733 s     0.060 s     0.055 s
//   decay T40           n/a      0.120 s     0.120 s
//   centroid        512.6 Hz    419.7 Hz    403.0 Hz
//   rolloff 85%         n/a     559.9 Hz    585.9 Hz
//   band 400-1000       n/a       47.1 %      45.9 %
//
// The "fundamental" is not quoted because it stopped meaning anything once the
// wobble was right: the voice alternates between two pitches rather than
// holding one, so a single-peak estimator reports whichever of them happens to
// dominate. It reads 286 Hz here against the reference's 338, and both are
// picking a different tone out of the same 280/440 pair.
//
// The corner stays where the schematic-derived model had it. Dropping it to
// 460 Hz to chase the centroid moved that number by 11 Hz, because what
// dominates it is the oscillator's pitch rather than the filter, and a corner
// below the fundamental only attenuates the note.
//
// What is left is one distribution difference, and it is the whole of the
// remaining distance: the reference puts 15 % of its energy below 150 Hz where
// this puts 2 %. The board's footstep has low-frequency content this model has
// no source for, and no filter corner produces it because there is nothing to
// filter. Structural, so recorded rather than tuned; see
// `…-dkong-walk-not-sustained-4q42`. Above that the two now agree closely —
// 400-1000 Hz lands within 1.2 points, and the decay matches to the
// millisecond at T40.
const WALK_LP_HZ: f64 = 700.0;
/// Re-fitted after the envelope changed: a decaying footstep carries far less
/// energy than the sustained tone this used to be, so the level had to come up
/// by ~12 dB to match the same reference.
///
/// Raised again by 5.4 dB when the mixer's own low-pass and the amplifier's
/// high-passes were added below it. Only the level moved — the centroid landed
/// on the reference's (400.6 Hz against 403.0) without touching `WALK_LP_HZ`,
/// which is the sign that the new stage belongs where it was put rather than
/// duplicating this one.
const WALK_GAIN: f64 = 1.072;
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
/// Wobble rate: a CMOS inverter oscillator around 4.3 kΩ and 10 µF, so its
/// timing capacitor relaxes with τ = 43 ms. Measured on the board the period is
/// 87 ms — 2.02 τ, and 11.5 Hz.
///
/// This was 1.16 Hz, taken from the RC corner rather than from the relaxation
/// period — the same mistake jump's 8.4 Hz was. A tenth of the real rate does
/// not read as a slow wobble; it reads as no wobble at all, because a footstep
/// lasts 160 ms and 1.16 Hz moves by a few percent in that time. Against the
/// board's two-level alternation between 280 and 440 Hz this gave a single
/// smooth glide from 432 down to 262 and stayed there.
///
/// Jump's oscillator has the same shape and settles at 1.85 τ; this one takes
/// 2.02. They differ because the ratio depends on where the inverter's
/// effective switching thresholds land, and this one has two inverters in
/// cascade where jump has three. Deriving that needs the transfer curve the
/// model does not carry, so as with jump the ratio is measured.
const WALK_LFO_HZ: f64 = 11.49;
/// Wobble depth in volts, from the oscillator's rail-to-rail swing through its
/// 12 kΩ into the ~2.25 kΩ the CV currents see: 2.4 V × 2251.8/12000.
///
/// What reaches the 555 is smaller than this, and should be: the 3.3 µF CV
/// capacitor only has 43 ms to slew, so it covers about 90 % of the step. The
/// board measures a CV swinging 3.35 to 4.12 V against the 3.745 ± 0.450 this
/// implies before the slew, which is the cross-check that the depth and the
/// slew are both right.
const WALK_CV_SWING: f64 = 0.450;
/// Walk footstep decay. The reference falls 20 dB in 55 ms, so τ ≈ 55 ms / ln(10).
const WALK_TAU: f64 = 0.024;
/// How long one footstep runs before it is cut off — many time constants, so the
/// envelope decays away on its own rather than being truncated.
const WALK_HOLD_S: f64 = 0.4;
// Jump, against a single trigger (`drive_dkong_single.lua` with DK_EFFECT=jump
// vs `sndcmp capture dkong/jump`). "before" is the previous committed model:
//
//                     before        now      reference
//   AC RMS         -31.00 dB   -34.58 dB    -34.67 dB
//   decay T20         0.145 s     0.419 s      0.495 s
//   decay T40         1.477 s     0.459 s      0.500 s
//   centroid         15.4 Hz    415.5 Hz     379.7 Hz
//   fundamental     347.2 Hz    361.5 Hz     347.8 Hz
//   band 0-150 Hz     97.7 %       2.2 %        3.7 %
//   band 150-400      1.7 %       51.6 %       69.0 %
//   band 400-1000     0.5 %       45.5 %       27.1 %
//   STFT distance      5.885       3.348            —
//
// The pitch trajectory now tracks the reference cycle for cycle: the wobble's
// period measures 0.109 s against 0.110 s and it carries the note between 320
// and 485 Hz against 325 and 485.
//
// The remaining gap is the 150-400 / 400-1000 split, and it is a waveform
// shape difference rather than a pitch one. Plotted against the reference this
// model's cycle has a genuinely FLAT top where the board's is rounded
// throughout — the emitter follower reaches its target and sits there for the
// rest of the square's high phase, and a flat top with a sharp corner carries
// harmonics the board's does not. The missing rounding is somewhere in the
// follower's response; it is structural, so it is recorded rather than closed
// with a filter at a fitted corner. See `…-0fbi`.
//
/// Output calibration, not a hardware value: it maps this circuit's volts into
/// the finite PCM range alongside the other effects.
///
/// The jump chain works in volts end to end, so this is the only scalar in it
/// that is not a component value. Everything upstream — the 555's 4.5 V high,
/// the 5 V lid, the diode drops, the follower's Vbe — is an absolute voltage,
/// and they only combine correctly on one shared scale. The earlier normalized
/// version (555 high = 1) had to fold each of those into a ratio, and two of
/// the ratios were wrong in ways that cancelled in the level and not in the
/// spectrum.
const JUMP_GAIN: f64 = 0.648;
// Stomp is a low rumble, not a hiss, and the rumble is a COUNTER — not a filter.
//
// The board clocks a 24-bit shift register at 4 kHz and feeds a counter that
// divides its rising edges by eight, taking the top bit. That alone lands the
// pitch near 125 Hz, which is where the reference's centroid sits. Everything
// after it is jump's chain, stage for stage and mostly value for value: the
// same ~26 ms conditioned trigger, the same diode-mixed lid, the same emitter
// follower into the same divider.
//
// This used to be a one-shot noise burst with a fitted exponential envelope
// into a fitted one-pole low-pass, which is the same shape by coincidence and
// not by mechanism. A low-pass on white noise gives a spectral tilt with no
// fundamental; a divided edge stream gives a square whose period wanders with
// the noise's run lengths. They can be made to measure the same centroid and
// they do not sound the same, and the old model's tail could never be made to
// fit — the comment here used to conclude that "the reference falls faster than
// one pole allows, which points at a second filter stage on the board." There
// is no second filter. The decay was never a filter's.
//
//                     before        now      reference
//   AC RMS         -23.89 dB  -23.89 dB      -23.89 dB
//   decay T20         0.279 s    0.519 s        0.545 s
//   decay T40         1.098 s    0.688 s        0.695 s
//   centroid        133.0 Hz   135.0 Hz       124.9 Hz
//   rolloff 85%     193.8 Hz   150.7 Hz       140.6 Hz
//   fundamental      88.1 Hz   132.0 Hz       125.0 Hz
//   band 0-150         61.8 %     72.3 %         89.5 %
//   band 150-400       37.2 %     27.1 %         10.2 %
//
// The decay is the headline: it was 49 % short at T20 and 58 % long at T40, and
// is now within 5 % and 1 %. That is what comes of the envelope and the
// follower being present rather than approximated by a one-pole filter.
//
// The rumble's rate is exact — the divided source measures 124.7 Hz against the
// board's 125.0, which is 4 kHz of shift-register edges at one rising edge in
// four, divided by eight. So the pitch is right and the remaining 150-400 Hz
// excess is harmonic content above it, not a mistuned source.
//
// That excess is the SAME residual jump has, in the same direction, and both
// voices reach the output through `rc_integrate`. Two independently rebuilt
// chains showing one signature is good evidence it is one cause in the shared
// follower rather than two coincidences; see `…-0fbi`.
//
// Note the multi-resolution STFT distance reads WORSE than the old model's
// (3.95 against 3.21) while every band and envelope measure improved. That is
// the metric, not the model: for a noise-derived voice it penalises a
// fundamental sitting a few Hz off more than it penalises having no fundamental
// at all, and the old filtered-noise model had none to misplace.
/// Noise clock, 4 kHz: the shift register is clocked by the video counter's 2VF,
/// which divides the 61.44 MHz master by 5 and 4 to get 1H, by 16 to get 16H, by
/// 12 and 2 to get 1VF, and by 2 again.
const STOMP_NOISE_HZ: f64 = 61_440_000.0 / 5.0 / 4.0 / 16.0 / 12.0 / 2.0 / 2.0;
/// The counter divides the noise's rising edges by eight and the board takes the
/// top bit, so one output period spans eight edges.
const STOMP_DIVISOR: u32 = 8;
/// Envelope lid: 3.3 µF pulled down through 10 kΩ and recovering through
/// 100 kΩ + 10 kΩ. Jump's asymmetry with jump's parts, a little faster.
const STOMP_ENV_DIP_S: f64 = 0.033;
const STOMP_ENV_RECOVER_S: f64 = 0.363;
/// Output stage, the same emitter follower jump has: 750 Ω into 1 µF against
/// 4.7 kΩ ∥ (2 kΩ + 5.1 kΩ), so 750 µs to charge and 3.6 ms to drain.
const STOMP_OUT_RE: f64 = 750.0;
/// Output divider, 5.1 kΩ of the 2 kΩ + 5.1 kΩ following the integrator — the
/// same network jump's output sees.
const STOMP_DIVIDER: f64 = 5.1 / 7.1;
/// Output calibration, the only scalar in the chain that is not a component
/// value. Re-derived from scratch when the structure changed; the old 9.54 was
/// calibrated against a model with no envelope and no follower in it.
const STOMP_GAIN: f64 = 0.546;

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
// Everything between the summing node and the speaker. Three couplings in
// series, and the one that matters is the last.
//
// The mixer's own output capacitor is the gentlest of the three, and modelling
// only that one left the board with a low end it does not have. What sets the
// real corner is the amplifier module's input: 1 kΩ against 4.7 µF, near 34 Hz.
// A voice whose envelope steps its DC level — jump, whose lid drops 2 V and
// takes half a second to climb back — puts a large slow transient into the
// mixer, and on the board that transient is what these stages exist to remove.
// With only a 3 Hz pole in the way it survives to the output and swamps the
// note it belongs to.
//
/// Mixer output coupling, 1 µF into the following stage's input impedance
/// (~100 kΩ) — about 1.6 Hz.
const MIX_AC_R: f64 = 100_000.0;
const MIX_AC_C: f64 = 1e-6;
/// The summing node's own capacitor: 100 nF across it, against the four 47 kΩ
/// input legs in parallel with the 10 kΩ feedback — about 5.4 kΩ, so a corner
/// near 295 Hz.
///
/// This sits right on top of the jump's fundamental, which is why leaving it
/// out is audible rather than subtle: every voice reached the output with its
/// full harmonic series, and the jump in particular carried three times the
/// reference's energy above 400 Hz. It is a *mixer* stage, not a per-effect
/// one — the effects were each given a private low-pass to stand in for it, and
/// those corners were fitted with this pole missing from underneath them.
const MIX_LP_R: f64 = 1.0 / (4.0 / 47_000.0 + 1.0 / 10_000.0);
const MIX_LP_C: f64 = 100e-9;
/// Amplifier interstage coupling, 50 kΩ / 33 µF. A tenth of a hertz: a DC block
/// and nothing more, but it is in the path.
const AMP_AC_R: f64 = 50_000.0;
const AMP_AC_C: f64 = 33e-6;
/// The amplifier stage's emitter bypass: 33 µF across the 150 Ω emitter leg, so
/// the stage has little gain below ~32 Hz and full gain above it. A second pole
/// within a few hertz of the coupling below, which is why the board's low end
/// falls away so much faster than one RC would.
const AMP_HP_R: f64 = 150.0;
const AMP_HP_C: f64 = 33e-6;
/// The amplifier module's input coupling, 1 kΩ / 4.7 µF ≈ 34 Hz. The board's
/// actual low-frequency limit.
const SPK_AC_R: f64 = 1_000.0;
const SPK_AC_C: f64 = 4.7e-6;
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
/// Jump's wobble rate: three CMOS inverters around 18 kΩ and 3.3 µF, so the
/// timing capacitor relaxes with τ = 59.4 ms and the period is a fixed multiple
/// of that. Measured on the board it is 110 ms — 1.85 τ, and 9.09 Hz.
///
/// The multiple is the part that cannot be read off the schematic. Taking the
/// inverter's datasheet switching thresholds (a third and two thirds of the
/// supply) gives 3.54 τ and half this rate; three inverters in cascade make the
/// composite transition far sharper than one, so the effective thresholds
/// collapse toward mid-supply and the capacitor turns round after a much shorter
/// swing. Deriving *where* they land needs the inverter's transfer curve, which
/// this model does not carry — so the ratio is measured rather than computed,
/// and it is the one number in this chain that is. Modelling the inverter chain
/// properly would replace it; see the design note.
const JUMP_LFO_HZ: f64 = 9.09;
/// Wobble depth in volts, from the oscillator's swing through its own 10 kΩ into
/// the ~2.3 kΩ the CV currents see.
const JUMP_CV_SWING: f64 = 0.575;
/// The jump 555's output-high level. Unlike walk, this chain runs in volts, so
/// it needs the real one rather than a normalized stand-in: it is compared
/// directly against a 5 V lid across two diodes, and the ~0.8 V by which the
/// square clears that lid is the difference of two absolute voltages. Getting
/// this wrong does not scale the note, it changes how much of it exists.
const JUMP_555_HIGH: f64 = VCC - 0.5;
/// 1N5553 forward drop at 1 mA.
const DIODE_V: f64 = 0.4;
/// The envelope reaches the summing node through one junction and the 555
/// through two. That 0.4 V asymmetry sets the crossover between them, so a
/// single shared drop is not a simplification — it moves where the note starts
/// and stops.
const JUMP_LID_DIODE_V: f64 = DIODE_V;
const JUMP_555_DIODE_V: f64 = DIODE_V * 2.0;
/// Output stage: an emitter follower, not a filter.
///
/// 150 Ω from the emitter into 1 µF, loaded by 4.7 kΩ ∥ (2 kΩ + 5.1 kΩ). The
/// transistor charges the cap in 150 µs and then cuts off, leaving it to drain
/// through ~2.8 kΩ in 3.0 ms — two time constants toward two different targets.
///
/// Modelling this as a plain 150 Ω/1 µF low-pass is what buried the note. A
/// low-pass settles on the square's *mean*, which makes the whole voice a slow
/// DC level following the envelope; the follower tracks the square's peaks and
/// sags between them, which is where the fundamental comes from.
const JUMP_OUT_VBE: f64 = 0.7;
const JUMP_OUT_RE: f64 = 150.0;
const JUMP_OUT_RLOAD: f64 = 4_700.0 * 7_100.0 / (4_700.0 + 7_100.0);
const JUMP_OUT_C: f64 = 1e-6;
/// Output divider, 5.1 kΩ of the 2 kΩ + 5.1 kΩ that follows the integrator.
const JUMP_DIVIDER: f64 = 5.1 / 7.1;
/// The lid charges toward the 5 V supply, which is above the 555's high — so at
/// rest it closes over the oscillator completely.
const JUMP_LID_REST_V: f64 = VCC;
/// Conditioned trigger width.
///
/// The latch edge is differentiated by 1 µF through 10 kΩ into another 10 kΩ,
/// so the cap relaxes with τ = 20 ms, and the comparator watches the divider tap
/// at half the cap's swing — 2.2 V at the edge — passing it while it stays above
/// 0.6 V. That is 20 ms · ln(2.2/0.6) ≈ 26 ms, and it does not depend on how
/// long the game holds the line.
const JUMP_TRIG_S: f64 = 0.026;
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
    // An oscillator wobbles the 555's control voltage, and it is where walk's
    // alternating two-tone character comes from. This is easy to mistake for a
    // bug: a footstep lasts tens of milliseconds and catches the wobble wherever
    // it happens to be, so measuring one step in isolation gives a pitch that
    // appears to drift run to run. It is not drift — the board really does vary
    // the pitch per step, and removing the wobble to "stabilise" it gives a
    // single repeated tone that is audibly wrong however precisely its frequency
    // is fitted.
    //
    // The wobble is a SQUARE, not a triangle: what feeds the control voltage is
    // the output of a CMOS inverter chain, which is a logic level. So the board
    // alternates between two pitches — measured, 280 Hz and 440 Hz — rather than
    // gliding through the range between them. The 3.3 µF control-voltage
    // capacitor rounds each step over ~7.4 ms, and that rounding is the whole of
    // the pitch movement audible *within* a single footstep.
    //
    // The board's square is not symmetric: it measures about 40 % high and 60 %
    // low. That asymmetry is real — this oscillator's bias resistor is only ten
    // times its timing resistor where jump's is a hundred and eighty, so its two
    // half cycles see meaningfully different resistance. Left as an even square
    // because the duty is a smaller effect than the rate and the shape, and
    // carrying it means a primitive with a duty parameter; recorded rather than
    // approximated by detuning the frequency to compensate.
    //
    // The control voltage is a capacitor, not a switch.
    //
    // Three currents feed the 555's CV pin through a 3.3 µF cap: a fixed one
    // from the chip's own divider, one gated by the walk latch, and one from the
    // wobble oscillator. The cap makes the CV *slew* between its levels with a
    // ~7.4 ms time constant rather than stepping.
    //
    // That slew is the piece every earlier attempt was missing. Modelling the CV
    // as instantaneous gives a steady pitch per step, which measures plausibly
    // and sounds wrong; a hold-based capture cannot reveal it because after two
    // seconds the cap has long since settled.
    let walk_lfo = b.fixed_square("WALK_LFO", WALK_LFO_HZ);
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
    // at 4.5 V, so the lid has headroom to close fully over the oscillator — and
    // the note ends at the finite moment it crosses the peak. Resting exactly at
    // the peak instead makes the lid approach it asymptotically, so the note
    // never quite stops and the decay measures several times too long however
    // the time constants are set.
    //
    // Reaching the node, the lid loses one diode drop and the square loses two,
    // so the crossing happens 0.4 V earlier than the bare levels suggest: the
    // 26 ms trigger leaves the lid at 2.9 V, the square peaks 1.2 V above it,
    // and the note ends ~440 ms later when the recovering lid reaches 4.1 V.
    let jump_lid_rest = b.constant("JUMP_LID_REST", JUMP_LID_REST_V);
    let jump_lid_dip = b.gain("JUMP_LID_DIP", jump_trig, -JUMP_LID_REST_V);
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
    // The wobble is a SQUARE, not a triangle. What reaches the control voltage
    // is the third inverter's output, which is a logic level — so the note
    // alternates between two pitches rather than gliding between them. The
    // 10 µF control-voltage capacitor rounds its edges over ~23 ms, which is
    // where the glide that is audible comes from; a triangle source produces a
    // continuous sweep instead, and smears the note's energy up into the
    // harmonics rather than holding it at two fundamentals.
    let jump_lfo = b.fixed_square("JUMP_LFO", JUMP_LFO_HZ);
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
        JUMP_555_HIGH,
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
    // Both inputs must share a scale for a max() to mean anything, and here that
    // scale is volts — the lid's 5 V rail against the 555's 4.5 V high, each
    // less its own diode drop.
    let jump_mixed = b.diode_mixer_drops(
        "JUMP_MIX",
        &[(jump_lid, JUMP_LID_DIODE_V), (jump_555, JUMP_555_DIODE_V)],
    );

    // Output stage: a transistor buffering the summing node into 1 µF, then the
    // divider that feeds the board mixer.
    //
    // No coupling capacitor here. Walk's chain has one; jump's does not — it
    // runs diode mixer straight into the follower and on to the board mixer,
    // where the shared amplifier couplings remove the DC.
    //
    // The DC is substantial and it is not an artifact: the lid resting high
    // holds this node near 4 V at rest, and the trigger steps it down and lets
    // it climb back over half a second. That step is real on the board too. What
    // removes it is the amplifier's 34 Hz input coupling, not anything in this
    // chain — putting a high-pass here instead makes the step ring at whatever
    // corner is chosen and leaves the note buried under it.
    let jump_int = b.rc_integrate(
        "JUMP_INT",
        jump_mixed,
        JUMP_OUT_VBE,
        JUMP_OUT_RE,
        JUMP_OUT_RLOAD,
        JUMP_OUT_C,
    );
    let jump = b.gain("JUMP_OUT", jump_int, JUMP_GAIN * JUMP_DIVIDER);

    // Stomp is jump's chain with a different source: free-running noise divided
    // down to a rumble, held off the output by a lid until the latch fires.
    //
    // The source is a shift register clocked at 4 kHz whose edges a counter
    // divides by eight. The counter is what makes this a rumble — its output is
    // a square at roughly an eighth of the noise's edge rate, with a period that
    // wanders as the noise's run lengths do. Low-passing the raw noise to the
    // same average frequency measures a similar centroid and sounds like hiss,
    // because it has no fundamental to hear.
    let stomp_noise = b.lfsr_noise(
        "STOMP_NOISE",
        STOMP_NOISE_HZ,
        LfsrSpec {
            width: 24,
            taps: (10, 23),
            seed: 0x1A_CFFC,
        },
    );
    let stomp_div = b.edge_divider("STOMP_DIV", stomp_noise, STOMP_DIVISOR);
    let stomp_src = b.gain("STOMP_SRC", stomp_div, VCC);

    // From here it is jump's chain, and mostly jump's values. The trigger is the
    // same differentiator into the same comparator (10 kΩ + 10 kΩ, 1 µF), so it
    // is the same ~26 ms pulse and equally indifferent to the latch's width.
    let stomp_trig = b.custom(
        "STOMP_TRIG",
        vec![stomp_en.into()],
        Box::new(DkongEdgePulse {
            width: JUMP_TRIG_S,
            timer: 0.0,
            active: false,
            last_en: false,
        }),
    );
    let stomp_lid_rest = b.constant("STOMP_LID_REST", JUMP_LID_REST_V);
    let stomp_lid_dip = b.gain("STOMP_LID_DIP", stomp_trig, -JUMP_LID_REST_V);
    let stomp_lid_target = b.add("STOMP_LID_TARGET", &[stomp_lid_rest, stomp_lid_dip]);
    let stomp_lid = b.rc_envelope(
        "STOMP_LID",
        stomp_lid_target,
        STOMP_ENV_RECOVER_S,
        STOMP_ENV_DIP_S,
    );
    let stomp_mixed = b.diode_mixer_drops(
        "STOMP_MIX",
        &[(stomp_lid, JUMP_LID_DIODE_V), (stomp_src, JUMP_555_DIODE_V)],
    );
    let stomp_int = b.rc_integrate(
        "STOMP_INT",
        stomp_mixed,
        JUMP_OUT_VBE,
        STOMP_OUT_RE,
        JUMP_OUT_RLOAD,
        JUMP_OUT_C,
    );
    let stomp = b.gain("STOMP_OUT", stomp_int, STOMP_GAIN * STOMP_DIVIDER);

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
    // This used to be a single 3 Hz pole, on the reasoning that the board's
    // output coupling uses the same 4.7 µF part as walk's. The capacitor is the
    // same; the resistor it works against is not. Walk's sees 11.2 kΩ, the
    // amplifier's input sees 1 kΩ — an order of magnitude, and the difference
    // between a board that reproduces sub-audio transients and one that does
    // not.
    let mix_lp = b.rc_low_pass("MIX_LP", mix, MIX_LP_R, MIX_LP_C);
    let mix_ac = b.rc_high_pass("MIX_AC", mix_lp, MIX_AC_R, MIX_AC_C);
    let amp_ac = b.rc_high_pass("AMP_AC", mix_ac, AMP_AC_R, AMP_AC_C);
    // The amplifier stage itself, as its emitter bypass: its gain rolls off
    // below ~32 Hz. Only the linear half is here — the transistor's clipping
    // ceiling is not modelled.
    let amp = b.rc_high_pass("AMP_HP", amp_ac, AMP_HP_R, AMP_HP_C);
    let out_ac = b.rc_high_pass("OUT_AC", amp, SPK_AC_R, SPK_AC_C);
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
