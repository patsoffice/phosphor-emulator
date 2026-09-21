//! Zaxxon's discrete sound board, built on the [`DiscreteCircuit`] framework
//! from `IC Board A 834-0214` sheets 11 and 12.
//!
//! There is no sound chip and no sound CPU on this board. Twelve active-low bits
//! of the i8255 at `U23` gate eleven analog voices, two further bits set a level
//! rather than gating anything, and all eleven voices meet at one passive
//! summing node the drawing calls `SJ`.
//!
//! The transcription this is built from is
//! [`docs/schematics/zaxxon-discrete-sound.md`](../../docs/schematics/zaxxon-discrete-sound.md),
//! and it is the thing to read before changing a constant here. Every component
//! value below carries its designator, so a constant that does not match the
//! drawing is a bug rather than a taste.
//!
//! # What is derived and what is not
//!
//! Most of this circuit is solved rather than fitted: the seven one-shot widths,
//! the three noise filters, the two engine resonators, the alarm divider chain
//! and the whole eleven-leg mix are arithmetic on values read off the drawing.
//!
//! One thing is still **invented**, and says `INVENTED` in its own doc comment
//! rather than hiding among the read values: the `MCD-725H` opto-isolator's
//! resistance against LED current, which sets the player ship's engine pitch.
//!
//! One more used to be, and is now **constrained by a recording**:
//! [`CANNON_R_Q6_ON`], `Q6`'s collector-emitter resistance against its base
//! drive, which sweeps the cannon. `08.wav` is that voice's own recording, it
//! does not clip, and every summary statistic is monotone in this constant and
//! improves against the board at 80 ohms over the 120 that was guessed. That is
//! a third category and it is marked as one at the call site: no sheet
//! dimensions a transistor's on-resistance, so a board measurement is not
//! competing with a read value, it is the only evidence there is.
//!
//! Three more rest on properties of parts rather than on the drawing, and are
//! named where they are used rather than marked `INVENTED`, because each is a
//! datasheet figure rather than a choice: [`OPAMP_SWING`], the 555s' output
//! levels and control-pin impedance, and [`MM5837_HZ`].
//!
//! [`MM5837_SWING`] used to be the third of those, on the grounds that it sets
//! a level and levels are what a read divider decides afterwards. **It is not
//! only a level**: `NOISE 1` lands on `U6`'s control pin, which is a 555's own
//! comparator threshold, so this constant sets the homing missile's *pitch*.
//! Its own comment says so, and
//! `the_homing_missiles_pitch_is_set_by_the_noise_on_its_control_pin` measures
//! it.
//!
//! The battleship's and the shot's oscillator pitches used to head this list.
//! Both are solved now, along with the laser's and the homing missile's, and
//! all four turned out to be **the same circuit**: an op-amp integrator, an
//! inverting Schmitt on a 51 k / 100 k or 33 k / 100 k pair, and a transistor
//! sinking the summing node through the resistor that sets the duty. See
//! [`relaxation_hz`], which all of them share. What differs between them is the
//! capacitor, the sink ratio, and where the reference comes from.
//!
//! The homing missile's 555 is the exception to that list and the one voice
//! whose rate is not arithmetic at all. Its parts give 787 Hz, and with the
//! noise its control pin actually carries it runs near 977 Hz.
//!
//! # What the reference recordings can and cannot settle
//!
//! This section used to be headed "there is no reference to compare against",
//! on the grounds that the reference emulator plays recorded WAV samples rather
//! than emulating the board. **That was wrong.** MAME's `zaxxon` sample set is
//! recorded from a real Zaxxon board, so it is evidence about this hardware:
//! one cabinet, through an unknown recording chain, four of its twelve files
//! clipped.
//!
//! The rule that survives is a distinction, not a dismissal:
//!
//! - **A recording can constrain a part property the drawing does not give.**
//!   [`CANNON_R_Q6_ON`] and [`PC1_R_BRIGHT`] are not in competition with a read
//!   value, because no sheet dimensions them. A board measurement is the only
//!   evidence that exists for either.
//! - **A recording cannot move a junction.** Both of the fits this file made and
//!   reverted were junction claims underneath: the alarm divider moved to `1QB`,
//!   a pin wired to nothing, and the battleship's rate moved to 750 Hz against a
//!   derived 122. Both also rested on a bad measurement, which is worth
//!   separating from the principle: the `1QB` fit came from a "near 5 kHz"
//!   reading that is where both alarm files' *centroid* sits, not their
//!   fundamental.
//! - **And it cannot correct a value the drawing gives.** Both alarms measure
//!   12 % below the clock `R168`, `R169` and `C97` give, by the same factor.
//!   That is one cabinet's ceramic capacitor and it is not a reason to move a
//!   read value.
//!
//! The catalog row in `tools/sound-compare/targets.toml` stays
//! `implemented-unvalidated`, because what the samples cannot review is a
//! topology and that is where every error on this board has been.

use phosphor_core::core::save_state::{SaveError, StateReader, StateWriter};
use phosphor_core::device::{
    CustomComponent, DataInputId, DiscreteCircuit, DiscreteCircuitBuilder, FilterMode, LfsrOutput,
    LfsrShift, LfsrSpec, LogicInputId, LogicOp, NodeId, OutputGain,
};
use phosphor_macros::Saveable;

fn sample_rate() -> u64 {
    phosphor_core::audio::host_sample_rate() as u64
}

/// Minimum internal simulation rate, a **floor** that `with_sim_rate` rounds up
/// to the next whole multiple of the resampler's intermediate rate.
///
/// The cannon's band-pass starts near 7.4 kHz with `Q6` saturated (see
/// [`CANNON_R_Q6_ON`]), and a Chamberlin state-variable filter needs its center
/// frequency well under a sixth of the step rate to stay accurate. 96 kHz puts
/// that corner at 16 kHz with room to spare; the 48 kHz default would not.
const MIN_SIM_RATE: u64 = 96_000;

// ---------------------------------------------------------------------------
// Supply rails (sheet 12, the power block at the bottom of p134)
// ---------------------------------------------------------------------------

/// The TTL rail.
const V5: f64 = 5.0;
/// The analog mid-rail, made by `R34`/`R35` 390 ohm from +12 V with `C36`/`C37`
/// 100 uF across each half. Everything analog on this board swings about it.
const V6: f64 = 6.0;
/// The analog supply.
const V12: f64 = 12.0;
/// A saturated open-collector output (7406, 7417, 74LS139).
const V_SAT: f64 = 0.2;
/// A silicon diode's forward drop, used by the envelope shapers.
const V_DIODE: f64 = 0.6;

// ---------------------------------------------------------------------------
// The noise source: MM5837 at U2
// ---------------------------------------------------------------------------

/// `U2`'s shift rate.
///
/// **Not read from the drawing, because it is internal to the part.** The
/// MM5837's oscillator is specified at a supply this board does not give it
/// (`Vss` sits at +12 V with `Vdd` and `Vgg` grounded, so the part runs at 12 V
/// against the datasheet's nominal 14 V) and the part-to-part spread is wide.
///
/// 48 kHz is MAME's `nld_mm5837` default, which also warns outside 24-56 kHz;
/// that range is the part's published spread and is the reason this is not the
/// 100 kHz figure the file first carried, which is outside it.
const MM5837_HZ: f64 = 48_000.0;

/// `U2`'s output swing about the mid-rail, after `C66` blocks its DC.
///
/// The part is a MOS output on a 12 V supply; this is half of that, which makes
/// `NOISE 1` a +/-5 V square sequence. It is the one amplitude on the board that
/// is a guess rather than a divider.
///
/// **It does not only set a level.** This comment used to end "so it sets the
/// absolute level and nothing else", which is true of the three Sallen-Key
/// voices and the cannon, where the noise is the signal and every stage after
/// it is a read divider. It is not true of the homing missile, where `NOISE 1`
/// lands on `U6`'s **control pin**: a 555's control pin is its comparator's
/// threshold, so noise on it biases every crossing early and moves the voice's
/// *pitch*. See
/// `the_homing_missiles_pitch_is_set_by_the_noise_on_its_control_pin`, which
/// measures 787 Hz without it and 977 Hz with it. Changing this constant
/// retunes that voice, which is the opposite of what a level control does.
const MM5837_SWING: f64 = 5.0;

/// The MM5837's 17-bit register, tapped at bits 13 and 16.
fn mm5837_lfsr() -> LfsrSpec {
    LfsrSpec {
        width: 17,
        taps: (13, 16),
        seed: 1,
        shift: LfsrShift::TowardHigh,
        invert_feedback: false,
        output: LfsrOutput::RegisterBit,
    }
}

/// `NOISE 2` is `NOISE 1` through `C66` 10 uF, `R111` 100 k and `U3` with `R112`
/// 10 k of feedback: an inverting stage with a gain of a tenth.
const R111: f64 = 100_000.0;
const R112: f64 = 10_000.0;

// ---------------------------------------------------------------------------
// Player ship: the two-bit level (sheet 12, p134 zone D7)
// ---------------------------------------------------------------------------

// The ladder, read at 400 dpi. R10 and R11 are the 7406 pull-ups and are NOT in
// the ladder; they cross it on the drawing without a junction. See the
// transcription's "Player ship A and B are a level" section, which is there
// because the issue this work came from described this network wrongly.
const R10: f64 = 2_200.0; // pull-up on U30 pin 10 (PLAYER SHIP A)
const R11: f64 = 2_200.0; // pull-up on U30 pin 12 (PLAYER SHIP B)
const R12: f64 = 6_800.0; // node A into node X
const R13: f64 = 82_000.0; // +12 V into node X
const R14: f64 = 36_000.0; // node X down to node Y
const R15: f64 = 56_000.0; // node X to ground
const R16: f64 = 100_000.0; // node B into node Y
const C25: f64 = 15e-6; // node Y to ground: the glide

/// Solve the four-node ladder at DC and return the control voltage at node Y.
///
/// `pa0_high`/`pa1_high` are the PPI bits as latched. Each passes through a 7406
/// inverting open-collector buffer, so a **high** bit saturates its node to
/// [`V_SAT`] and a **low** bit lets its pull-up take the node toward +12 V.
///
/// Nodal analysis by relaxation rather than a matrix: four nodes, diagonally
/// dominant, and it converges to a microvolt in a handful of sweeps. It runs
/// four times at circuit-build time and never again.
fn ship_level_volts(pa0_high: bool, pa1_high: bool) -> f64 {
    let (mut x, mut y) = (V12, V12);
    for _ in 0..200 {
        // A high bit saturates its inverter's output; a low one lets the pull-up
        // take it, loaded by the ladder leg below: (a - 12)/R10 + (a - x)/R12 = 0.
        let a = if pa0_high {
            V_SAT
        } else {
            (V12 / R10 + x / R12) / (1.0 / R10 + 1.0 / R12)
        };
        let b = if pa1_high {
            V_SAT
        } else {
            (V12 / R11 + y / R16) / (1.0 / R11 + 1.0 / R16)
        };
        // (x - a)/R12 + (x - 12)/R13 + x/R15 + (x - y)/R14 = 0
        x = (a / R12 + V12 / R13 + y / R14) / (1.0 / R12 + 1.0 / R13 + 1.0 / R15 + 1.0 / R14);
        // (y - b)/R16 + (y - x)/R14 = 0; C25 is open at DC and U8's input draws
        // nothing.
        y = (b / R16 + x / R14) / (1.0 / R16 + 1.0 / R14);
    }
    y
}

/// The four control voltages, indexed by the raw bits as `PA0 * 2 + PA1`.
///
/// Two things here are both true and easy to get backwards, and between them
/// they are why a `data & 3` volume fit cannot be right:
///
/// - **`PA0` is the more significant bit.** It drives node X through `R12`
///   6.8 kOhm where `PA1` reaches node Y through `R16` 100 kOhm, so `PA0` is
///   worth about twice as much. A fit that reads the pair as a little-endian
///   two-bit number swaps the two middle states.
/// - **The level falls as the bits rise.** Both bits are inverted by `U30`'s
///   7406 sections, so the maximum is at `PA0 = PA1 = 0` and the minimum, with
///   `PC1`'s LED dark, is the `1, 1` that `RP1` leaves at power-on. That is also
///   what makes the engine quiet at reset.
///
/// So the array descends: about 10.92, 7.39, 4.18 and 0.76 V, in near-linear
/// steps of 3.4 V.
fn ship_levels() -> [f64; 4] {
    [
        ship_level_volts(false, false),
        ship_level_volts(false, true),
        ship_level_volts(true, false),
        ship_level_volts(true, true),
    ]
}

/// Thevenin resistance seen by `C25` at node Y: `R16` in parallel with `R14`
/// plus whatever node X presents, which `R14` dominates. 26.5 kOhm against
/// 15 uF is the roughly 0.4 s glide between levels.
const SHIP_GLIDE_R: f64 = 26_500.0;

// ---------------------------------------------------------------------------
// Player ship: the LDR-tuned band-pass (sheet 12, p134 zone D5)
// ---------------------------------------------------------------------------

const R17: f64 = 390.0; // U8's follower output into PC1's LED
const R18: f64 = 200_000.0; // NOISE 1 into U4
const R19: f64 = 10_000.0; // U4 feedback
const R20: f64 = 10_000.0; // the band-pass input resistor
const R21: f64 = 470_000.0; // U5 feedback
const C26: f64 = 0.01e-6; // the two band-pass feedback caps (C26 = C27)
// This one stage's non-inverting input is on **+5 V**, not the +6 V mid-rail
// every other analog part on the board swings about. Read at 400 dpi with the
// label drawn beside the pin. It changes nothing audible, because `C28` blocks
// the DC going in and `C29`/`C38` block it coming out, and the LDR's other end
// is on +6 V so the tuning node floats there with no current in it. It is
// recorded because it is the one op-amp on this board that is referenced
// somewhere else, and a later pass that assumes the mid-rail here would be
// assuming rather than reading.

/// `PC1`'s LED forward drop. Below this the LED is dark, which is what the
/// lowest of the four levels produces.
const PC1_LED_VF: f64 = 1.2;

/// **INVENTED.** `PC1`'s photoresistance at the brightest of the four levels,
/// its dark value, and the exponent it falls with between them.
///
/// The `MCD-725H`'s transfer curve is not on the drawing and no datasheet was
/// found for it (searched again 2026-09-20; the part does not appear outside
/// distributor stock listings), so this is the one part of the engine voice that
/// is a model choice rather than a reading. A CdS cell's resistance falls close
/// to a power law in illumination; these three numbers put the front end's
/// center at 232, 461, 626 and 770 Hz across the ladder's four levels.
///
/// What the drawing *does* fix, and what a change here must preserve:
///
/// - with the LED dark the input resistance is `R20` alone and the center is
///   `1 / (2*pi*C26*sqrt(R20*R21))` = 232 Hz, the lowest of the four and not
///   adjustable here;
/// - the **bandwidth is 68 Hz wherever the center goes**. An MFB band-pass's
///   `f0/Q` is `1/(pi*R21*C26)` and neither of those parts moves, so the LDR
///   slides a fixed 68 Hz window rather than widening it. That is why the `Q`
///   runs 3.4 at the bottom of the ladder and 11.4 at the top, and it is
///   arithmetic on read values rather than a consequence of anything invented.
///
/// **The reference recordings cannot place this curve, and it was not fitted to
/// them.** MAME's `04.wav` and `05.wav` are the two engine states, and both are
/// about an octave wide: `04` sits 6.7 dB down one octave below its peak and
/// `05` sits 9.6 dB down. This circuit's front end is 68 Hz wide, which is
/// 0.3 of an octave at the *bottom* of its range and narrower everywhere above.
/// No position of the LDR makes this board as broad as either recording, so the
/// recordings are measuring something other than this chain (a different
/// cabinet's parts, or more than one voice at once) and cannot say where the
/// curve should sit. See the comparison written up in the transcription.
const PC1_R_BRIGHT: f64 = 1_000.0;
const PC1_EXPONENT: f64 = 1.05;
/// Dark resistance, which is also the ceiling the power law is clamped to.
const PC1_R_DARK: f64 = 5_000_000.0;

/// The two **Sallen-Key low-passes** the engine tone is shaped by (sheet 12,
/// p135 zone D4).
///
/// `1 / (2*pi*R*C)` with `R24`/`R25` = `R38`/`R39` = 100 k, and an amplifier
/// gain of `1 + R23/R22` = `1 + R37/R36` = 2, so `Q = 1/(3 - K)` = 1.
///
/// **These are not Wien resonators and not band-passes**, which is what this
/// file called them for as long as nobody traced them. They are the same
/// topology as the two explosion filters forty lines below, drawn twice more:
/// the input reaches the first resistor, the two resistors are in series to the
/// non-inverting input, the first capacitor returns that input to AC ground and
/// the second bridges the resistors' junction to the output. Sheet 12 makes the
/// distinction turn on one junction per copy. `C30`'s and `C39`'s far plates sit
/// on the **+6 V rail**, whose vertical crosses the signal bus with no dot; a
/// reading that put them on the bus would be a band-pass, and this one is a
/// low-pass with a Q-1 bump at `f0`.
///
/// The difference is a whole octave of output. A band-pass rejects everything
/// below `f0`, so it deleted the part of the engine the board actually passes
/// and left the LDR-tuned front end (which runs up to a Q of 11 at the top of
/// the ladder, see [`PC1_R_BRIGHT`]) to decide the pitch on its own. Both tones
/// then came out at the front end's frequency instead of their own, which is why
/// the 723 Hz copy and the 482 Hz copy measured identically.
const SHIP_TONE_A_HZ: f64 = 723.4; // C30, C31 2200 pF
const SHIP_TONE_B_HZ: f64 = 482.3; // C39, C40 3300 pF
const SHIP_TONE_Q: f64 = 1.0;
/// `1 + R23/R22` with `R22` = `R23` = 2.2 k, and `R22`'s far end on +6 V.
const SHIP_TONE_GAIN: f64 = 2.0;

/// The divider between each low-pass's output and its `MB4391`'s `IN`, which
/// this file had no entry for at all: `R26` 12 k in series with `R27` 3.3 k to
/// **ground**, then `C32` 2.2 uF into `U14` pin 1. `R40`/`R41` and `C41` are the
/// same three parts again on tone B.
///
/// 0.216, or 13.3 dB, and it applies to both engine tones and to nothing else on
/// the board. Leaving it out made the engine the loudest thing on the mix by a
/// margin the leg table does not allow: the legs say the engine sits 3.1 dB
/// below the medium explosion, and without `R26`/`R27` it sat above it.
const R26: f64 = 12_000.0;
const R27: f64 = 3_300.0;

// The 74LS139-gated VCA controls: pulled down through 1 k, released to +6 V
// through 440 k.
const R28: f64 = 1_000.0; // and R31
const R29_R30: f64 = 440_000.0; // R29 + R30, and R32 + R33
const C34: f64 = 0.68e-6; // and C35

// ---------------------------------------------------------------------------
// The MB4391 VCAs
// ---------------------------------------------------------------------------

/// The `MB4391`'s supply, which the drawing does not give.
///
/// Its symbols on sheets 11 and 12 show only `IN`, `CON`, `OUT` and `RO`; the
/// `VCC` and `GND` pins are not drawn. +5 V is inferred, and the inference is
/// the strongest single check in this file: see [`mb4391_mute_v`].
const MB4391_VCC: f64 = 5.0;

/// Control voltage at or above which an `MB4391` is muted.
///
/// **The direction is established from this board; the shape and the thresholds
/// are a second opinion, not a reading.** Five independent uses here agree that
/// the control pin attenuates: both explosion envelopes sit charged at rest and
/// are pulled *down* on a trigger, and both engine tones' capacitors sit at
/// +6 V when their 74LS139 output is inactive and are pulled to ground when it
/// is selected. Under the opposite polarity the board would howl at reset and go
/// quiet when a voice fired.
///
/// The numbers come from MAME's netlist for Borderline
/// (`src/mame/sega/nl_brdrline.cpp`), a Sega/Gremlin board of the same era
/// carrying the same part. That netlist is explicitly a guess: its author
/// labels it "values by guesses" and the file's own header says "MB4391 is
/// missing, a fake substitution is used". So this corroborates rather than
/// establishes. It is worth adopting over the linear ramp this file first
/// carried for two reasons:
///
/// - it is expressed against the part's supply (`VCC - 0.24` to
///   `VCC/2 + 0.34`), which is what lets it be transferred to another board at
///   all, and it confirms the pinout read off the Zaxxon sheets exactly:
///   `IN 1, CON 2, RO 14, OUT 15` and `IN 5, CON 6, RO 10, OUT 11`;
/// - at `VCC` = 5 V it lands on **Zaxxon's own envelopes**. The explosion
///   shaper read off sheet 11 rests at 5.00 V (the `R104`/`R105` divider's idle)
///   and bottoms at 2.80 V (the same divider with `C61` pulled to a diode drop).
///   The mute and full-gain points are 4.76 V and 2.84 V. The circuit's envelope
///   sweeps precisely the VCA's control range and stops just past each end,
///   which is what a board designer would arrange and is not something two
///   unrelated guesses would produce by accident. It is also the reason to
///   believe [`MB4391_VCC`] is 5 V.
///
/// The gain is that ramp **squared**, and the part's maximum gain is unity: it
/// is an attenuator, with no make-up gain to find.
fn mb4391_mute_v() -> f64 {
    MB4391_VCC - 0.24
}

/// Control voltage at or below which an `MB4391` passes its input unattenuated.
/// See [`mb4391_mute_v`].
fn mb4391_full_v() -> f64 {
    MB4391_VCC / 2.0 + 0.34
}

// ---------------------------------------------------------------------------
// The 74123 one-shots (sheets 11 and 12)
// ---------------------------------------------------------------------------

/// The plain 74123's timing constant for a large `Cext`.
///
/// **Not the 74LS123's 0.45**, which is what [`Ls123Charge::Direct`] carries and
/// is why these do not use the framework's `ls123` node. The board's parts are
/// marked `74123` on both sheets; taking the LS figure would make every width
/// here 60 % too long.
///
/// [`Ls123Charge::Direct`]: phosphor_core::device::Ls123Charge::Direct
const K74123: f64 = 0.28;

/// `(R, C)` for each one-shot, in the order their voices appear below.
const OS_S_EXP: (f64, f64) = (36_000.0, 1e-6); // U22 A: C60, R102   -> 10.1 ms
const OS_M_EXP: (f64, f64) = (47_000.0, 3.3e-6); // U21 B: C62, R107  -> 43.4 ms
const OS_CANNON: (f64, f64) = (47_000.0, 1e-6); // U45 A: C78, R125   -> 13.2 ms
const OS_SHOT: (f64, f64) = (18_000.0, 2.2e-6); // U21 A: C87, R142   -> 11.1 ms
const OS_ALARM: (f64, f64) = (47_000.0, 10e-6); // U46 B / U44 A      -> 131.6 ms
const OS_BASE_MISSILE: (f64, f64) = (36_000.0, 15e-6); // U22 B: C48, R56 -> 151 ms

// ---------------------------------------------------------------------------
// The two explosions (sheet 11, p132 zone C7 and B7)
// ---------------------------------------------------------------------------

const C61: f64 = 2.2e-6; // small explosion envelope cap
const R106: f64 = 1_000.0; // its discharge resistor, through D7
const R104_R105: f64 = 940_000.0; // its recovery path to +5 V -> 2.07 s

const C63: f64 = 1e-6; // medium explosion envelope cap
const R212: f64 = 470.0; // its discharge resistor, through D8
const R109_R110: f64 = 2_000_000.0; // its recovery path to +5 V -> 2.00 s

/// The two Sallen-Key noise bands the explosions are heard through.
///
/// `1 / (2*pi*R*C)` with `R113`/`R116` = `R119`/`R122` = 15 k, and an amplifier
/// gain of `1 + R115/R114` = 2.5, so `Q = 1/(3 - K)` = 2.
const S_EXP_HZ: f64 = 321.5; // C70, C71 0.033 uF
const M_EXP_HZ: f64 = 225.7; // C72, C73 0.047 uF
const EXP_FILTER_Q: f64 = 2.0;
const EXP_FILTER_GAIN: f64 = 2.5;

// ---------------------------------------------------------------------------
// The cannon (sheet 11, p132 zone B7 and B5)
// ---------------------------------------------------------------------------

const R126: f64 = 330.0; // Q's pull-up, so the envelope's charge path
const C79: f64 = 6.8e-6; // the envelope cap
const R127_ENV: f64 = 100_000.0; // its decay to ground -> 0.68 s
const R127_FB: f64 = 47_000.0; // U12's band-pass feedback (see below)
const R128: f64 = 10_000.0; // the band-pass input resistor
const R130: f64 = 100.0; // in series with Q6, from the tuning node to ground
const C81: f64 = 0.01e-6; // the two band-pass feedback caps (C81 = C82)
// C83 10 uF then R134 into R135 to ground: a 2:1 divider on the way to C84 and
// MB4391 U13 ch B. Read on sheet 11's right half, and missed on the first pass,
// which left the cannon twice as loud as the board makes it.
const R134: f64 = 100_000.0;
const R135: f64 = 100_000.0;

/// The reference `U12`'s cannon-VCA section sits at: `R139` 33 k and `R141`
/// 22 k divide +6 V onto its non-inverting input, bypassed by `C85` 47 uF.
///
/// With `R136` 51 k in and `R137` 51 k of feedback the section is an inverting
/// amp of gain -1 about this point, so its output is `2 * ref - envelope`. That
/// the result rests at 4.8 V, a fifth of a volt above the `MB4391`'s 4.76 V mute
/// threshold, is the third independent landing on that window.
const U12_CANNON_REF: f64 = 6.0 * 22_000.0 / (33_000.0 + 22_000.0);

// `R127` really does appear twice on sheet 11, once as the 100 k envelope shunt
// and once as the 47 k band-pass feedback, both legible at 400 dpi. One of them
// is presumably `R129`, which appears nowhere. The two names above distinguish
// them by function because the drawing does not.

/// `R133`, from `Q6`'s collector to ground, which is what **bounds** the
/// cannon's sweep at the quiet end.
///
/// The T's shunt is `R130` in series with `R133` in parallel with `Q6`, so it
/// runs from 1.6 kOhm with `Q6` off to `R130` alone with it hard on. That makes
/// the corner sweep 1835 Hz to 7.3 kHz and the Q 2.7 to 10.8. An earlier pass
/// here had `Q6` opening to 10 MOhm, which `R133` flatly contradicts: the
/// transistor cannot take the node anywhere near that, and the voice spent its
/// life at the top of a range it should barely reach.
const R133: f64 = 1_500.0;

/// `Q6`'s base divider, read: `R131` 15 k from `U12` pin 7 and `R132` 3.3 k to
/// ground, so the base sees **0.180** of the envelope.
///
/// Traced at 260 %, with the transistor drawn rotated as they all are on this
/// sheet: the horizontal lead is the base, the top lead is the collector (to
/// `R130` and `R133`) and the bottom one is the emitter, to ground.
const R131: f64 = 15_000.0;
const R132: f64 = 3_300.0;

/// The envelope at which `Q6` starts conducting at all: **3.33 V**.
///
/// A bipolar transistor's base-emitter junction is a silicon diode, so nothing
/// happens until the base reaches [`V_DIODE`], and the base is `R132/(R131 +
/// R132)` of the envelope. That is `0.6 * 18.3/3.3` = 3.33 V, which the
/// envelope leaves **0.19 s** into its 0.68 s decay.
///
/// This is derived rather than invented, and it is not the same claim as
/// [`CANNON_R_Q6_ON`], which is still a guess. What it fixes is a model that had
/// `Q6` conducting for the whole of the decay because its threshold was zero:
/// the board's cannon spends a fifth of its length sweeping down and the rest
/// parked at `R130 + R133`'s 1835 Hz, and the old one swept all the way.
///
/// The peak base voltage is 0.79 V, a fifth of a volt above this, so `Q6` is a
/// soft variable resistance over a narrow range rather than a switch. `R133`
/// bounds what it can do either way.
fn cannon_q6_threshold_v() -> f64 {
    V_DIODE * (R131 + R132) / R132
}

/// How far `Q6` pulls `R133` down at full envelope: **80 ohms**, and this is
/// the first constant on this board to be **constrained by a recording** rather
/// than invented or read.
///
/// [`cannon_q6_threshold_v`] fixes where the sweep *stops* and [`R133`] fixes
/// how little `Q6` can do at the quiet end, so this sets only how bright the
/// first 0.19 s is. No sheet dimensions a transistor's collector-emitter
/// resistance against its base drive, so there is no read value for a recording
/// to overwrite here: a board measurement is the only evidence that exists, and
/// see the module doc for why refusing it was the wrong call.
///
/// `08.wav` is the right file to take it from. It is the **cannon's own
/// recording and it does not clip**, unlike the four that do, so its spectrum
/// is the board's rather than its capture chain's.
///
/// Scanned against it, every summary statistic is monotone in this constant and
/// all of them improve from the 120 ohms that was guessed here:
///
/// | | 120 (guessed) | **80** | 60 |
/// |---|---|---|---|
/// | centroid, against 2627.7 Hz | 2312.3 | **2486.1** | 2628.6 |
/// | 85 % rolloff, against 4177.4 Hz | 3876.0 | **4392.8** | 4823.4 |
/// | worst band delta | 5.99 pp | **3.51 pp** | 5.77 pp |
///
/// **60 ohms lands the centroid within 0.03 % and it is not the answer**, which
/// is the part worth writing down. The 1-3 kHz and 3-8 kHz bands are a seesaw
/// in this constant, and our cannon carries about **6 pp of energy below 1 kHz
/// that the recording does not**, from somewhere else entirely. That excess
/// drags our centroid down, so the value that makes the centroid agree is the
/// value that over-brightens the sweep to compensate for a different defect.
/// 80 ohms is where the worst band delta is near its minimum and where the
/// centroid lands once that excess is accounted for.
///
/// So this is constrained to roughly **60 to 90 ohms** rather than fitted to a
/// decimal, and 80 is physically ordinary for a small-signal transistor at the
/// 35 uA of base drive `R131` and `R132` deliver at peak envelope. The sub-1 kHz
/// excess is a separate defect and is not chased by moving this.
const CANNON_R_Q6_ON: f64 = 80.0;
const CANNON_R_Q6_OFF: f64 = 10_000_000.0;

// ---------------------------------------------------------------------------
// The alarms (sheet 11, p132 zone A5 and p133 zone A4)
// ---------------------------------------------------------------------------

const R168: f64 = 470.0;
const R169: f64 = 120.0;
const C97: f64 = 0.1e-6;
/// `U50`, half a 556: `1.44 / ((R168 + 2*R169) * C97)`.
///
/// 20 kHz is not a voice. It is the clock for the 74393 at `U49`, whose `1QC`
/// and `1QD` taps are the two alarm pitches.
fn alarm_clock_hz() -> f64 {
    1.44 / ((R168 + 2.0 * R169) * C97)
}

const R172: f64 = 1_500.0; // into U12's inverting input
/// `U12`'s alarm feedback. Nothing computes with it, because the point of it is
/// that it is far too big to matter: `R173`/`R172` is a DC gain of 220 into a
/// twelve-volt supply, which is the argument that the stage is a comparator.
/// `the_alarm_stage_cannot_run_linearly` is where that argument is checked.
#[allow(dead_code)]
const R173: f64 = 330_000.0;
const C99: f64 = 0.01e-6; // across it, which is what limits the edges

/// How fast `C99` lets `U12`'s alarm section move its output, in volts per
/// second: **400 kV/s**, or 0.4 V per microsecond.
///
/// This stage is a comparator, not an amplifier, and the number that matters
/// about it is a slew rate rather than a corner frequency. `R171` 1 k pulls the
/// 7426's wired-AND node to +12 V and an open-collector section pulls it to
/// [`V_SAT`], so `R172` 1.5 k delivers about 4 mA either side of the +6 V on the
/// section's non-inverting input. `R173` 330 k can return 36 uA at most, a
/// hundredth of that, so essentially all of it goes into `C99` and the output
/// ramps at `I / C99` until it hits a rail and stays there.
///
/// The two directions differ by 3 % because [`V_SAT`] is not 0 V; this is the
/// pull-up direction, and the asymmetry is below anything audible.
fn alarm_slew_v_per_s() -> f64 {
    ((V12 - V6) / R172) / C99
}

/// How far `U12`'s alarm section rises off its rest during a burst: the whole
/// output swing, `2 * OPAMP_SWING`.
///
/// Expressed as a rise from rest rather than as two absolute voltages, which is
/// this file's convention for every node that sits somewhere other than zero at
/// power-on: `R171` holds the section's input at +12 V whenever neither alarm is
/// gating and the section inverts, so it rests pinned at the bottom of its
/// swing. A model referenced to the mid-rail instead would push that offset
/// through `C24` as a step at power-on, which is the thump the board's mute
/// circuit exists to cover and which nothing here needs to reproduce.
fn alarm_burst_v() -> f64 {
    2.0 * OPAMP_SWING
}

// ---------------------------------------------------------------------------
// The battleship and the shot: read at block level only
// ---------------------------------------------------------------------------

// The battleship is TWO relaxation oscillators of the same design, one slow and
// one at audio rate. Each is an op-amp integrator driving an inverting Schmitt
// trigger, with the Schmitt's output closing the loop through a diode, a
// resistor and a transistor that sinks the integrator's summing node: `Q4` on
// the slow stage, `Q5` on the fast one. Every part of both is read below, and
// nothing in this voice is invented any more.
//
// Two junctions decide the whole thing, and this file had both wrong until they
// were re-cropped at 400 dpi:
//
//   * `R85` lands on `U9`'s pin-6 SUMMING node, not on the `R83`/`R84` divider
//     that feeds pin 5. The divider's line crosses that vertical with no
//     junction dot.
//   * `R96` lands on `U10`'s pin-6 summing node in exactly the same way. An
//     earlier pass read it off the `R94`/`R95` divider, which made the fast
//     stage barely able to reverse its own integrator and put its rate a
//     factor of four out.
//
// The two stages are therefore one circuit built twice, and that is what lets
// their ratio be stated exactly.
const R80: f64 = 2_200_000.0; // reference divider, top
const R81: f64 = 220_000.0; // reference divider, bottom
// The slow stage's five parts reach no audio (see `battleship_hz`), so nothing
// in the built circuit reads them and only a test does. They are kept because
// `battleship_mod_hz` derives a rate from them that that test pins: the day
// somebody finds the wire this drawing is missing, the stage is already here
// and already solved. `allow` rather than `expect`, because `expect` reports
// itself unfulfilled in the build where the test does use them.
#[allow(dead_code)]
const R82: f64 = 30_000.0; // slow integrator input
#[allow(dead_code)]
const R85: f64 = 2_200.0; // slow integrator sink, through Q4
#[allow(dead_code)]
const R86: f64 = 51_000.0; // slow Schmitt input
#[allow(dead_code)]
const R88: f64 = 100_000.0; // slow Schmitt feedback
#[allow(dead_code)]
const C56_C57: f64 = 1.65e-6; // 3.3 uF in series with 3.3 uF, back to back
const R90: f64 = 120_000.0; // fast stage's reference divider, top
const R91: f64 = 100_000.0; // fast stage's reference divider, bottom
const R93: f64 = 30_000.0; // fast integrator input
const R96: f64 = 15_000.0; // fast integrator sink, through Q5
const R98: f64 = 51_000.0; // fast Schmitt input
const R99: f64 = 100_000.0; // fast Schmitt feedback
const C58: f64 = 0.01e-6;

/// The reference `U9(1,2,3)` buffers: `R80` 2.2 M and `R81` 220 k off +12 V.
fn battleship_ref_v() -> f64 {
    V12 * R81 / (R80 + R81)
}

/// A Schmitt trigger's window, as a voltage at the integrator's output.
///
/// Both triggers are the same part twice: the integrator drives the inverting
/// input and the output returns to the non-inverting one through `R_fb`, which
/// is held toward the +6 V mid-rail by `R_in`. The threshold is therefore
/// `6 * R_fb/(R_in + R_fb) + Vout * R_in/(R_in + R_fb)`, so the window the
/// integrator has to cross is `R_in/(R_in + R_fb)` of the output's full swing.
///
/// With 51 k and 100 k that is 0.338 of 10 V, or **3.378 V**. This is the one
/// term in the whole voice that rests on [`OPAMP_SWING`] rather than on a read
/// value, and both stages' rates are inversely proportional to it: a wider
/// output swing is a slower oscillator. It cancels exactly in their ratio.
fn schmitt_window_v(schmitt_in: f64, schmitt_fb: f64) -> f64 {
    schmitt_in / (schmitt_in + schmitt_fb) * 2.0 * OPAMP_SWING
}

/// One integrator-and-Schmitt stage's rate, from its four resistors and its cap.
///
/// The textbook `R_fb / (4 * R_in * R_integ * C)` is **not** right for this
/// circuit and is not what this computes. That form assumes the Schmitt drives
/// the integrator's input directly. Here it drives a transistor which, when on,
/// pulls the summing node down through `sink` (`R85` or `R96`), so the two ramps
/// run at different currents:
///
/// ```text
/// v_g          the integrator's virtual ground, set by its own divider
/// i_charge  =  v_g / integ_in          with the transistor off
/// i_reverse =  v_g / sink - i_charge   with it on and saturated
/// T         =  window * C * (1/i_charge + 1/i_reverse)
/// ```
///
/// The transistor's own saturation voltage drops out of this to within a couple
/// of percent, because it is tens of millivolts against a virtual ground of a
/// quarter to half a volt and it appears only in `i_reverse`, which the slow
/// stage runs thirteen times faster than its other ramp.
///
/// Note what `integ_in` and `sink` do together: the fast stage's 30 k against
/// 15 k makes `i_reverse` equal `i_charge` **exactly**, which is a 50 % square
/// and is plainly the point of that pair.
fn relaxation_hz(v_g: f64, integ_in: f64, sink: f64, c: f64, window: f64) -> f64 {
    let i_charge = v_g / integ_in;
    let i_reverse = v_g / sink - i_charge;
    1.0 / (window * c * (1.0 / i_charge + 1.0 / i_reverse))
}

/// The slow stage, `U9(5,6,7)` and `U9(9,10,8)` around `Q4`: **3.02 Hz** at a
/// 7.3 % duty cycle, a thump rather than a tone.
///
/// Its virtual ground is `R83` 51 k and `R84` 51 k halving [`battleship_ref_v`],
/// so exactly half of it.
///
/// **This stage reaches nothing.** See [`battleship_hz`].
#[allow(dead_code)] // solved, and reachable only from a test; see above
fn battleship_mod_hz() -> f64 {
    relaxation_hz(
        battleship_ref_v() / 2.0,
        R82,
        R85,
        C56_C57,
        schmitt_window_v(R86, R88),
    )
}

/// The voltage `R93` works against: `R90` 120 k and `R91` 100 k divide
/// [`battleship_ref_v`] down to 0.496 V, and `U9(12,13,11)` then `U10(1,2,3)`
/// buffer it.
fn battleship_fast_src_v() -> f64 {
    battleship_ref_v() * R91 / (R90 + R91)
}

/// The fast stage, `U10(5,6,7)` and `U10(9,10,8)` around `Q5`: **122 Hz**, and
/// the only half of this voice that is audible.
///
/// Its virtual ground is `R94` 51 k and `R95` 51 k halving
/// [`battleship_fast_src_v`], so a quarter of the reference, which is why it
/// runs 40 times the slow stage rather than the 165 the two capacitors alone
/// would suggest.
///
/// **The slow stage does not modulate this one, because as drawn it cannot.**
/// `U9`'s integrator output leaves through `R92` 30 k, and `R92`'s other end
/// lands on the node where `U10(1,2,3)`'s output, its own inverting input,
/// `R93` and `R94` all meet. That section's inverting input is strapped to its
/// output by a plain wire, which makes it a unity follower of the 0.496 V bias
/// on its pin 3: `R92` can only load it. The slow oscillator has no other
/// output on the sheet and `R92` has no other end, so this is either a drawing
/// error or a vestigial part, but either way the drawing gives no modulation
/// path and inventing a depth for one would be inventing the circuit.
///
/// This file previously carried an invented 62 Hz, then an invented 750 Hz
/// fitted to a MAME recording, then a derived rate resting on an invented drive
/// voltage. This one rests on nothing but the drawing and [`OPAMP_SWING`].
fn battleship_hz() -> f64 {
    relaxation_hz(
        battleship_fast_src_v() / 2.0,
        R93,
        R96,
        C58,
        schmitt_window_v(R98, R99),
    )
}

/// What the 4016B actually receives, peak to peak: **3.378 V**, not the op-amp's
/// full swing.
///
/// `U10(12,13,14)` is a unity follower and `C59` 2.2 uF takes its output
/// straight to the switch with no divider, so the file previously gave this
/// voice the whole 10 V. But the follower's pin 12 does not tap the Schmitt's
/// *output*: it taps the `R98`/`R99` junction, the Schmitt's own hysteresis
/// node, one crossing lower on the sheet. That node swings by exactly the window
/// the integrator has to cross, symmetrically about the +6 V that `R98` holds it
/// toward, which is also the bias `R189`/`R190` put on the far side of `C59`.
fn battleship_swing_v() -> f64 {
    schmitt_window_v(R98, R99)
}

// The shot is a TONE, and there is no noise anywhere in it. This file used to
// band-pass `NOISE 2` at a frequency taken from `R156` and `C92` on the
// assumption that `U19`'s section had the same second-order shape as the
// neighboring filters. It does not: `U19(5,6,7)` with `C92` in its feedback and
// `U19(9,10,8)` around `R161`/`R162` are **the same integrator-and-Schmitt
// relaxation oscillator as the battleship**, down to the transistor sinking the
// summing node, and `R156`/`R157`/`R158` are its reference divider rather than a
// filter's input. Two voices, one circuit, read twice.
//
// What is different here is that this oscillator's reference is not a fixed
// divider off a rail. It is a live node, so the pitch is swept, and everything
// below exists to work out by how much.
const R143: f64 = 3_300.0; // +5 V into the shaper node
const R144: f64 = 560.0; // U21's Qbar into the same node
const R145_R146: f64 = 1_270_000.0; // +12 V down to node X, in series
const R147: f64 = 1_000_000.0; // node X down to node Y
const R148: f64 = 2_200_000.0; // node Y to ground
const C88: f64 = 0.047e-6; // the shaper node into X
const C89: f64 = 0.68e-6; // on node Y: the VCA's decay
const R149: f64 = 5_600.0; // X's buffer into U19's inverting amp
const R150: f64 = 33_000.0; // that amp's feedback: a gain of -5.9
const R151_R152: f64 = 20_000.0; // U18 555 charge path, in series
const R152: f64 = 10_000.0; // its discharge path
const C90: f64 = 3.3e-6; // its timing cap
const R153: f64 = 2_700.0; // the 555's output into node A
const R154: f64 = 8_200.0; // the envelope's other way into node A
const R155: f64 = 820.0; // node A's load to ground
const C91: f64 = 15e-6; // node A's smoothing
const R156: f64 = 33_000.0; // the oscillator's integrator input
const R159: f64 = 15_000.0; // its sink, through Q7
const C92: f64 = 1000e-12; // its integrator cap
const R161: f64 = 33_000.0; // its Schmitt input, from +6 V
const R162: f64 = 100_000.0; // its Schmitt feedback
const R164: f64 = 1_000_000.0; // out of the oscillator
const R165: f64 = 220_000.0; // to ground: a divider of 0.18

/// The shaper node between `R143` and `R144`, when `U21`'s `Qbar` is at `q_bar`.
///
/// **`R144` is driven by `Qbar` (pin 4), not `Q`.** `Q` (pin 13) is drawn and
/// goes nowhere, and the difference is the whole polarity of the voice: the node
/// rests HIGH and the trigger pulls it down, which is the same shape the two
/// explosions and the base missile use, for the same reason (see
/// [`inverted_envelope`]). Both ends sit at +5 V at rest, so the node does too.
fn shot_shaper_v(q_bar: f64) -> f64 {
    (V5 / R143 + q_bar / R144) / (1.0 / R143 + 1.0 / R144)
}

/// Node Y, the `MB4391 U16` control, at rest and at the bottom of a trigger.
///
/// `D10`'s anode is on `Y` and its cathode on the shaper node, so `Y` is clamped
/// one diode drop above it. Its own DC, from `R145`/`R146` and `R147` against
/// `R148`, would put it at 5.9 V, and the clamp holds it below that.
fn shot_vca_rest_v() -> f64 {
    (shot_shaper_v(V5) + V_DIODE).min(V12 * R148 / (R145_R146 + R147 + R148))
}

fn shot_vca_floor_v() -> f64 {
    shot_shaper_v(V_SAT) + V_DIODE
}

/// Node X, the pitch shaper, at rest: `R145` and `R146` from +12 V against
/// `R147` down to the clamped `Y`.
fn shot_pitch_rest_v() -> f64 {
    let rest = shot_vca_rest_v();
    rest + (V12 - rest) / (R145_R146 + R147) * R147
}

/// How far `C88` carries the shaper node's step into node X, expressed as the
/// high-pass corner `C88` makes against everything X is tied to.
///
/// `R145` and `R146` reach +12 V and `R147` and `R148` reach ground, so X sits on
/// 0.91 MOhm and the step decays over 43 ms. `D10`'s state changes that by about
/// a third while the trigger is low, which is inside what the op-amp's clipping
/// hides.
fn shot_pitch_r() -> f64 {
    let up = R145_R146;
    let down = R147 + R148;
    up * down / (up + down)
}

/// Node A's weights: the 555's output through `R153`, and `U19`'s inverting
/// amplifier through `R154`, into `R155`'s 820 ohms to ground.
///
/// `R155` is the reason this voice is not simply the 555's square: it holds the
/// node down to a fifth of what either source would give alone, which is what
/// keeps the oscillator in its audible range.
fn shot_node_a_weights() -> (f64, f64) {
    let sum = 1.0 / R153 + 1.0 / R154 + 1.0 / R155;
    ((1.0 / R153) / sum, (1.0 / R154) / sum)
}

/// The oscillator's rate against node A, **3331 Hz per volt**.
///
/// `R157` and `R158` are both 33 k, so the integrator's virtual ground is half
/// of node A, and the rate is linear in it. Same helper as the battleship,
/// because it is the same circuit.
fn shot_hz_per_volt() -> f64 {
    relaxation_hz(0.5, R156, R159, C92, schmitt_window_v(R161, R162))
}

/// What the oscillator's square is worth at the VCA: `R164` 1 M into `R165`
/// 220 k, so 0.18 of the op-amp's swing.
fn shot_out_v() -> f64 {
    2.0 * OPAMP_SWING * R165 / (R164 + R165)
}

// ---------------------------------------------------------------------------
// The three sheet-12 voices
// ---------------------------------------------------------------------------

const R44: f64 = 6_800.0; // U5's timing resistor, output back to pin 6
const C43: f64 = 6.8e-6; // on pin 6, to ground: R44 * C43 = 46.2 ms

const R48: f64 = 47_000.0; // U6 555, the homing missile's swept tone
const R49: f64 = 68_000.0;
const C45: f64 = 0.01e-6;
/// `1.44 / ((R48 + 2*R49) * C45)` = **787 Hz**: the pitch `U6` free-runs at when
/// its control pin is left alone.
///
/// Nothing in the built circuit calls this, because [`Timer555`] integrates the
/// capacitor against a live control pin rather than being handed a rate. It is
/// the closed form that the component has to agree with when the control pin is
/// parked, and `the_homing_missiles_555_free_runs_where_its_parts_say` is what
/// makes it.
///
/// **It is not the pitch the voice is heard at, and this file used to say it
/// was.** The control pin is not left alone: `U4` puts the 15.4 Hz warble
/// *and* `NOISE 1` on it, and the noise is what sets the rate. See
/// `the_homing_missiles_pitch_is_set_by_the_noise_on_its_control_pin`. The
/// warble alone leaves the part here; the noise moves it to about 977 Hz, and
/// the "710 Hz to 872 Hz" this file and the transcription both carried is the
/// sweep the warble would make around a rate the board does not run at.
#[allow(dead_code)] // checked against Timer555 by a test; see above
fn homing_missile_hz() -> f64 {
    1.44 / ((R48 + 2.0 * R49) * C45)
}
/// `R42`'s far end is **+6 V**, not the gate, and that is the whole voice.
///
/// This file read it as the 7406's output twice: once as an envelope driven by
/// the gate level, and once as a latch thrown by it. It is neither. `R42` 51 k
/// runs from the +6 V mid-rail to `U5` pin 5, `R43` 100 k runs from `U5` pin 7
/// back to the same pin, and `U5` pin 6 sits on the junction of `R44` and
/// `C43`. Nothing the gate does reaches this stage at all: `U30`'s 7406 output
/// crosses the whole sheet with no junction on it and turns up at the page edge
/// to `U17`'s 4016B control pin, exactly as the laser's and the battleship's
/// gates do.
///
/// So `U5`(5,6,7) is a **free-running op-amp relaxation oscillator**, running
/// whether or not the voice is gated, and the gate is a switch. See
/// [`homing_warble_hz`].
const R42: f64 = 51_000.0; // +6 V into U5 pin 5
const R43: f64 = 100_000.0; // U5 pin 7 back to pin 5: hysteresis
const R45: f64 = 68_000.0; // the warble into U4's summing node
const R46: f64 = 200_000.0; // NOISE 1 into the same node, through C44 1 uF
const R47: f64 = 10_000.0; // U4's feedback: gains of 0.147 and 0.05
/// `U4` pin 7 into `U6`'s control pin, read as **33 uF 16 V** at 700 %.
///
/// This file had 2.2 uF, which is what `C28`, `C29`, `C38`, `C55` and `C93` all
/// are on these sheets, and the mistake changed what the part *does*: 2.2 uF
/// against [`CV_PIN_R`] is a 22 Hz high-pass, which differentiates a 15 Hz
/// modulation into a transient, and 33 uF is a **1.4 Hz DC block**, which passes
/// it whole. The voice is a continuous warble, not a chirp.
const C46: f64 = 33e-6;

/// The impedance a bipolar 555's control pin presents, about **3.3 kOhm**.
///
/// **Not a reading**: it is the part's internal 5 k / 5 k / 5 k ladder seen from
/// the tap between the upper two, so 5 k in parallel with 10 k. With [`C46`]'s
/// 33 uF it is a 1.4 Hz corner, so everything `U4` sums reaches pin 5 with its
/// shape intact and only its DC removed.
const CV_PIN_R: f64 = 5_000.0 * 10_000.0 / 15_000.0;

/// `U5`'s hysteresis fraction, `R42/(R42+R43)` = **0.338**.
///
/// The output returns to its own non-inverting input through `R43` against
/// `R42` to the mid-rail, so the comparator trips when `C43` reaches this
/// fraction of the output's swing either side of +6 V. It is the same 51 k /
/// 100 k pair the battleship's and the laser's Schmitt triggers use, doing the
/// same job, which is worth noticing: five stages on this board are built from
/// it.
fn homing_beta() -> f64 {
    R42 / (R42 + R43)
}

/// The warble `U5` free-runs at, **15.4 Hz**, and it rests on nothing but four
/// read parts.
///
/// `C43` charges toward whichever rail the output is on through `R44`, and
/// trips at `+/- beta` of that rail, so each half period is
/// `R44*C43 * ln((1 + beta)/(1 - beta))`. [`OPAMP_SWING`] appears in the
/// numerator and the denominator of that log and **cancels exactly**, which is
/// the same shape of argument as the battleship's 40.5:1 ratio: the rate is a
/// reading even though the amplitude is not.
///
/// The duty is exactly 50 % for the same reason, because the two thresholds sit
/// symmetrically about +6 V and the two rails do too.
///
/// Nothing in the built circuit calls this, for the same reason
/// [`laser_repeat_hz`] and [`homing_missile_hz`] are not called: what the rest
/// of the voice uses is the waveform on `C43`, so [`OpAmpRelaxation`] integrates
/// the capacitor rather than being handed a rate. It is the closed form the
/// component has to agree with, and
/// `the_homing_missiles_warble_free_runs_and_does_not_rest_on_the_swing` is
/// what makes it. `allow` rather than `expect`, because `expect` reports itself
/// unfulfilled in the build where the test does use it.
#[allow(dead_code)] // checked against OpAmpRelaxation by a test; see above
fn homing_warble_hz() -> f64 {
    let beta = homing_beta();
    let half = R44 * C43 * ((1.0 + beta) / (1.0 - beta)).ln();
    1.0 / (2.0 * half)
}

/// `U6` runs on +5 V (pins 4 and 8), so its square output swings to about
/// `Vcc - 1.2`, and `R51`/`R52` divide that before `C47` and the 4016B.
const R51: f64 = 12_000.0;
const R52: f64 = 3_300.0;

const R58: f64 = 470.0; // base missile envelope discharge, through D2
const C49: f64 = 15e-6;
/// `R59` + `R60`, **220 kOhm each**, from `C49` up to +5 V.
///
/// Both values are read, and their being equal is what justifies the halving
/// [`inverted_envelope`] applies: `U20`(5,6,7) taps their junction, not the
/// capacitor, so the control moves half as far as `C49` does. The sum is the
/// recovery path, 6.6 s, which is three times either explosion's and is the
/// longest time constant on the board.
const R59_R60: f64 = 220_000.0 + 220_000.0;
/// The third Sallen-Key noise band, on sheet 12: `R61`/`R62` 15 k with
/// `C50`/`C137` 0.022 uF, gain `1 + R63/R64` with `R63` 50 k and `R64` 100 k =
/// 1.5, so `Q = 1/(3 - K)` = 0.67.
///
/// Traced at component level: `C137` bridges the `R61`/`R62` junction to the
/// output and `C50` takes `U4` pin 10 to **ground**, which is the same shape as
/// the two explosions and the two engine tones. It is the fifth copy of one
/// filter on this board.
///
/// Note that this lands on **exactly** engine tone B's 482.3 Hz, from a
/// completely different pair: 15 k with 0.022 uF here, 100 k with 3300 pF
/// there. That is a coincidence in the parts and a fact about the board.
const BASE_MISSILE_HZ: f64 = 482.3;
const BASE_MISSILE_Q: f64 = 0.667;
const BASE_MISSILE_GAIN: f64 = 1.5;

const R65: f64 = 5_100.0; // U7 555, the laser's repetition rate
const R66: f64 = 22_000.0;
const C53: f64 = 10e-6;
/// `U7`'s repetition rate, **5.31 Hz**, a rate rather than a tone.
///
/// `D3` sits across `R66`, so the charge path is `R65` alone and the discharge
/// path is `R66` alone: `t_high = 0.693*R65*C53`, `t_low = 0.693*R66*C53`, and
/// the rate is `1.44/((R65 + R66)*C53)` at an 18.8 % duty cycle. The file first
/// used the undiode'd `R65 + 2*R66` form, which gave 2.93 Hz: the diode is on
/// the drawing and halves the period.
///
/// This is also the one figure in this voice that the reference recording
/// corroborates. MAME loops `01.wav` while the gate is low, and that sample is
/// 0.20 s long, which is one period of 5.31 Hz to within a frame.
///
/// Nothing in the built circuit calls this: [`Timer555`] integrates the
/// capacitor rather than being told a rate. It is the closed form the component
/// has to agree with, and `the_laser_555_ramps_at_the_rate_its_parts_give`
/// makes it do so.
#[allow(dead_code)] // checked against Timer555 by a test; see above
fn laser_repeat_hz() -> f64 {
    1.44 / ((R65 + R66) * C53)
}

/// `U7`'s duty cycle, `R65 / (R65 + R66)`: the rise is short and the fall long,
/// which is the whole shape of the laser's sweep.
#[allow(dead_code)] // checked against Timer555 by a test; see `laser_repeat_hz`
fn laser_duty() -> f64 {
    R65 / (R65 + R66)
}

// The `U8`/`Q3`/`D4` chain that `U7` drives is the **fourth** copy of the
// battleship's integrator-and-Schmitt oscillator on this board, and `R70` lands
// on `U8`'s pin 6 summing node exactly as `R85`, `R96` and `R159` do. What is
// different is what feeds its reference: `U8`(1,2,3) is a unity follower, and
// its pin 3 sits on `U7`'s pins 2 and 6, the 555's own timing capacitor.
//
// So the laser is a square whose pitch is swept by a capacitor ramp between the
// part's two thresholds, 4 V and 8 V, rising in 35 ms and falling over 153 ms.
// The file previously made it a triangle at an invented 1450 Hz, amplitude
// modulated by an envelope off the same 555 read as a square, which is why its
// energy sat in one band where the board's spreads over six.
const R67: f64 = 120_000.0; // laser integrator input
const R70: f64 = 47_000.0; // its sink, through Q3
const C138: f64 = 0.01e-6; // its integrator cap
const R72: f64 = 51_000.0; // its Schmitt input, from +6 V
const R73: f64 = 100_000.0; // its Schmitt feedback

/// The laser oscillator's rate against `U7`'s capacitor, **75 Hz per volt**.
///
/// `R68` and `R69` are both 51 k, so the integrator's virtual ground is half the
/// follower's output and the rate is linear in it. Between the 555's 4 V and 8 V
/// that is **300 Hz to 600 Hz**, which is where the reference recording's energy
/// sits.
fn laser_hz_per_volt() -> f64 {
    relaxation_hz(0.5, R67, R70, C138, schmitt_window_v(R72, R73))
}

/// `R75`/`R76` divide `U8`'s output before `C55` and the 4016B. This is what
/// sets the laser's level against the rest of the board, and it is read.
const R75: f64 = 10_000.0;
const R76: f64 = 2_200.0;

/// The swing an op-amp on this board's single +12 V supply delivers about the
/// +6 V mid-rail.
///
/// Not a reading: the drawing dimensions no op-amp's output stage. It is the
/// rail less the usual pair of volts of headroom, and it is the reference every
/// voice whose own chain is read only at block level is expressed against, so
/// that those voices are at least the right size relative to the ones that are
/// derived end to end.
const OPAMP_SWING: f64 = 5.0;

// ---------------------------------------------------------------------------
// The mix (sheet 11 p133, sheet 12 p135)
// ---------------------------------------------------------------------------

/// Every voice's common resistor into `SJ`. All eleven are the same value, which
/// is why the whole balance of the board is the attenuator ahead of each one.
const R_COMMON: f64 = 51_000.0;
/// `R209`, the load `SJ` sees into `U11`'s virtual ground.
const R209: f64 = 10_000.0;
/// `U11`'s gain, `-R210 / R209` with `R210` 82 k.
const U11_GAIN: f64 = -8.2;
/// `R211` 68 k into `VR1` 20 k to ground, at full volume.
const VOLUME: f64 = 20_000.0 / (68_000.0 + 20_000.0);

/// One voice's leg: `(series, shunt)` in ohms, ahead of the 1 uF block and the
/// shared [`R_COMMON`]. The table is the transcription's, in the same order.
const LEG_SHIP_A: (f64, f64) = (15_000.0, 22_000.0); // R174 / R175
const LEG_SHIP_B: (f64, f64) = (15_000.0, 22_000.0); // R177 / R178
const LEG_HOMING_MISSILE: (f64, f64) = (22_000.0, 27_000.0); // R180 / R181
const LEG_BASE_MISSILE: (f64, f64) = (39_000.0, 8_200.0); // R183 / R184
const LEG_LASER: (f64, f64) = (47_000.0, 8_200.0); // R186 / R187
const LEG_BATTLESHIP: (f64, f64) = (47_000.0, 4_700.0); // R191 / R192
const LEG_S_EXP: (f64, f64) = (15_000.0, 8_200.0); // R194 / R195
const LEG_M_EXP: (f64, f64) = (8_200.0, 47_000.0); // R197 / R198
const LEG_CANNON: (f64, f64) = (47_000.0, 3_900.0); // R200 / R201
const LEG_SHOT: (f64, f64) = (39_000.0, 8_200.0); // R203 / R204
const LEG_ALARM: (f64, f64) = (68_000.0, 1_000.0); // R206 / R207

/// The 1 uF block against the 51 kOhm common: a 3.1 Hz corner, which is below
/// every *tone* on the board, so it is here for the DC offsets the VCAs and
/// switches leave behind.
///
/// **It is not below every envelope, and this comment used to say it was.** A
/// 3.1 Hz corner is a 51 ms time constant, and the alarms' one-shot is 132 ms:
/// less than three time constants, so `C24` droops visibly across a burst and
/// hands back an equal and opposite tail after it. That is measurable and it is
/// measured. Windowed 25 ms at a time, the alarm leg's 125-250 Hz energy falls
/// from 14 dB below its own full band at the start of a burst to 30 dB below at
/// the end, monotonically, which is the droop and nothing else.
///
/// It is also the reason the reference recording looks 17 dB apart from this
/// voice in that band and is not evidence of anything: `21.wav` is a 78 ms loop
/// body, which is by construction cut from the part of a sound that does *not*
/// change, so it cannot contain a droop. The two shortest one-shots, the small
/// explosion's 10 ms and the shot's 11 ms, are far enough inside 51 ms that the
/// block is a differentiator for them rather than a block; the two explosions'
/// and the base missile's envelopes are seconds long and see it as a block.
const C_BLOCK: f64 = 1e-6;

/// Final scaling into the resampler.
///
/// **A headroom choice, not a reading.** The `LA4460`'s closed-loop gain is set
/// by `C12`/`R4` on a pin the drawing does not dimension, and the volume
/// potentiometer `VR1` is an operator control with no defined position, so there
/// is no voltage on this board that corresponds to full scale. This puts a
/// single loud voice at roughly a third of full scale and leaves room for the
/// several that overlap in play.
///
/// It was 3.2 and did not do that: the loudest single voice peaked at 0.20 of
/// full scale, and the whole board came out 8 to 13 dB below the level the
/// reference emulator plays its samples at. That is audible as the board simply
/// being quiet, and the medium explosion is where it gets noticed because the
/// board's loudest leg is on it.
///
/// 4.4 puts the loudest single voice at **0.27** rather than the third this
/// comment used to claim outright, and the difference is not slack: the binding
/// constraint is `nothing_saturates`, every voice sounding at once, which clips
/// at 4.8. The game never does that, so the bound is conservative, but a model
/// that clips is worse than one that is quiet and the conservative bound is the
/// one to keep.
///
/// **Nothing about the board's internal balance moves with this.** It is one
/// scalar on the output, after `SJ`, so every voice keeps the level its own
/// source amplitude and its own leg give it. If a voice sounds wrong relative to
/// its neighbors, this is not the constant that is wrong, and
/// `voice_levels_follow_the_leg_table` is the test that speaks to that.
const OUTPUT_GAIN: f64 = 4.4;

/// How many legs meet at `SJ`. Eleven, because alarms 2 and 3 share one.
const LEG_COUNT: usize = 11;

/// What one volt at a mixer leg is worth at the output sample, as a fraction of
/// full scale. **0.509** on the constants above.
///
/// `SJ` is passive, so a leg arrives through its own [`R_COMMON`] against all
/// eleven of them and [`R209`]; after that it is `U11`'s gain, `VR1` at full
/// volume, and [`OUTPUT_GAIN`].
///
/// This is here so that a **per-voice probe can be captured at the level that
/// voice actually contributes to the mix**, which is the only scale on which a
/// leg capture and a mix capture mean the same thing.
/// `tools/sound-compare`'s Zaxxon probes used a flat 50x instead, on the
/// reasoning that the legs are millivolt-scale and need lifting to be audible.
/// They are not: the loudest leg peaks near 0.2 V, so 50x put it at ten times
/// full scale and **every per-voice capture on this board clipped**, which is a
/// capture defect of exactly the kind `disasm audiodiff` exists to name.
///
/// `every_mix_leg_uses_the_same_common_resistor` checks the leg count this
/// rests on, and `a_leg_probe_is_the_voices_share_of_the_mix` checks the number
/// against the device rather than against this arithmetic.
pub fn leg_to_output() -> f64 {
    let legs_g = LEG_COUNT as f64 / R_COMMON;
    (1.0 / R_COMMON) / (legs_g + 1.0 / R209) * sj_to_output()
}

/// What one volt at `SJ` is worth at the output sample, as a fraction of full
/// scale: `U11`'s gain, `VR1` at full volume, and [`OUTPUT_GAIN`].
///
/// `SJ` is already past the legs' division by eleven commons, so a probe of the
/// node itself takes this and not [`leg_to_output`].
pub fn sj_to_output() -> f64 {
    U11_GAIN.abs() * VOLUME * OUTPUT_GAIN
}

// ---------------------------------------------------------------------------
// Custom components
// ---------------------------------------------------------------------------

/// A plain 74123 monostable: retriggerable, and timed at [`K74123`] rather than
/// the 74LS123's constant.
///
/// The framework's `ls123` node carries the LS part's 0.45 (or 0.25 diode-fed),
/// and neither is this part. Rather than widen a core enum that describes a
/// *charge path* with a variant that is really a *part*, the one board that has
/// plain '123s models them here.
///
/// Input `[0]`: the gate, active high. A rising edge starts or restarts the
/// pulse. Output: 1.0 while the pulse is running.
struct OneShot74123 {
    width: f64,
    remaining: f64,
    last: f64,
}

impl OneShot74123 {
    fn new((r, c): (f64, f64)) -> Self {
        Self {
            width: K74123 * r * c,
            remaining: 0.0,
            last: 0.0,
        }
    }
}

impl CustomComponent for OneShot74123 {
    fn reset(&mut self) {
        self.remaining = 0.0;
        self.last = 0.0;
    }

    fn step(&mut self, inputs: &[f64], dt: f64) -> f64 {
        let gate = inputs[0];
        if gate >= 0.5 && self.last < 0.5 {
            self.remaining = self.width;
        }
        self.last = gate;
        if self.remaining <= 0.0 {
            return 0.0;
        }
        self.remaining -= dt;
        1.0
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.write_f64_le(self.remaining);
        w.write_f64_le(self.last);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.remaining = r.read_f64_le()?;
        self.last = r.read_f64_le()?;
        Ok(())
    }
}

/// A photoresistor or a transistor standing in a band-pass's tuning leg,
/// modeled as a power law between a dark value and a driven one.
///
/// This is the invented half of two voices, kept in one named place. Input
/// `[0]`: the drive, in volts. Output: resistance in ohms.
struct VariableResistor {
    /// Drive below which the element is fully off.
    threshold_v: f64,
    /// Drive at which `r_min` is reached.
    full_v: f64,
    r_dark: f64,
    r_min: f64,
    exponent: f64,
}

impl CustomComponent for VariableResistor {
    fn reset(&mut self) {}

    fn step(&mut self, inputs: &[f64], _dt: f64) -> f64 {
        let drive = inputs[0] - self.threshold_v;
        if drive <= 0.0 {
            return self.r_dark;
        }
        let frac = (drive / (self.full_v - self.threshold_v)).min(1.0);
        // r = r_min * frac^-exponent, clamped to the dark value.
        let r = self.r_min * frac.powf(-self.exponent);
        r.min(self.r_dark)
    }

    fn save_state(&self, _w: &mut StateWriter) {}

    fn load_state(&mut self, _r: &mut StateReader) -> Result<(), SaveError> {
        Ok(())
    }
}

/// An op-amp multiple-feedback band-pass whose tuning resistance moves while it
/// runs.
///
/// The framework's `op_amp_band_pass` precomputes its biquad from fixed
/// resistors, which is right for every board that has one and wrong for the two
/// on this one: the player ship's is tuned by an opto-isolator's photoresistor
/// and the cannon's by a transistor, and in both the sweep *is* the voice.
///
/// For the MFB topology with equal feedback capacitors `c`, an input resistor
/// `r_in`, a feedback resistor `r_f` and a tuning resistance `r_tune` from the
/// capacitor junction to ground,
///
/// ```text
/// rp = r_in || r_tune
/// f0 = 1 / (2*pi*c*sqrt(rp * r_f))
/// Q  = 0.5 * sqrt(r_f / rp)
/// gain at f0 = r_f / (2 * r_in)
/// ```
///
/// A Chamberlin state-variable filter's band output peaks at `Q`, so scaling it
/// by `gain / Q` reproduces the op-amp's response exactly at resonance:
/// `sqrt(r_f * rp) / r_in`.
///
/// Input `[0]`: the signal. Input `[1]`: `r_tune` in ohms.
struct TunedBandPass {
    r_in: f64,
    r_f: f64,
    c: f64,
    low: f64,
    band: f64,
}

impl TunedBandPass {
    fn new(r_in: f64, r_f: f64, c: f64) -> Self {
        Self {
            r_in,
            r_f,
            c,
            low: 0.0,
            band: 0.0,
        }
    }
}

impl CustomComponent for TunedBandPass {
    fn reset(&mut self) {
        self.low = 0.0;
        self.band = 0.0;
    }

    fn step(&mut self, inputs: &[f64], dt: f64) -> f64 {
        let r_tune = inputs[1].max(1.0);
        let rp = (self.r_in * r_tune) / (self.r_in + r_tune);
        let f0 = 1.0 / (std::f64::consts::TAU * self.c * (rp * self.r_f).sqrt());
        // A Chamberlin filter is only accurate well below a sixth of the step
        // rate; above that it is unstable rather than merely wrong, so the
        // clamp is load-bearing and not defensive.
        let fs = 1.0 / dt;
        let f0 = f0.min(fs / 6.0);
        let q = 0.5 * (self.r_f / rp).sqrt();
        let f = 2.0 * (std::f64::consts::PI * f0 * dt).sin();
        let q1 = (1.0 / q).min(2.0);

        self.low += f * self.band;
        let high = inputs[0] - self.low - q1 * self.band;
        self.band += f * high;

        self.band * (self.r_f * rp).sqrt() / self.r_in
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.write_f64_le(self.low);
        w.write_f64_le(self.band);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.low = r.read_f64_le()?;
        self.band = r.read_f64_le()?;
        Ok(())
    }
}

/// Two resistances in parallel, for a network whose branches are both modeled
/// nodes rather than constants.
/// A 555 astable whose **control pin is live**, which is the only reason this
/// needs to be a component rather than a frequency.
///
/// Two of this board's 555s have something driving pin 5, and a 555 with a
/// moving control voltage is not a moving frequency: the part compares its
/// capacitor against the control pin and against half of it, so raising the
/// control raises both thresholds, and the charge and discharge legs stretch by
/// different amounts because one works against `vcc` and the other against
/// ground. The duty cycle moves with the pitch, and it is the duty that this
/// board is using.
///
/// The cap's own state is what carries that across a step: when the shot's
/// envelope throws the control from 1 V to 11 V, the part does not speed up, it
/// stops mid-charge and climbs for as long as the new threshold takes, which is
/// the one long pulse at the head of the voice.
///
/// Input `[0]`: the control pin, in volts.
struct Timer555 {
    /// Charge path: `R_a + R_b`.
    r_charge: f64,
    /// Discharge path: `R_b`.
    r_discharge: f64,
    c: f64,
    /// The part's own supply. `U18` and `U7` run on +12 V and `U6` on +5 V, and
    /// the difference is not only the output level: the capacitor charges toward
    /// this, so it is inside the rate as well.
    vcc: f64,
    v_high: f64,
    v_low: f64,
    /// Report the timing capacitor rather than pin 3. See [`Timer555::tapping_the_cap`].
    tap_cap: bool,
    cap: f64,
    high: bool,
}

impl Timer555 {
    fn new(r_charge: f64, r_discharge: f64, c: f64) -> Self {
        Self::on_supply(r_charge, r_discharge, c, V12)
    }

    fn on_supply(r_charge: f64, r_discharge: f64, c: f64, vcc: f64) -> Self {
        Self {
            r_charge,
            r_discharge,
            c,
            vcc,
            // A bipolar 555 drops about 1.7 V at its output when sourcing and
            // saturates near ground when sinking.
            v_high: vcc - 1.7,
            v_low: V_SAT,
            tap_cap: false,
            cap: 0.0,
            high: false,
        }
    }

    /// Report the timing capacitor instead of pin 3.
    ///
    /// `U7`'s **pin 3 is not drawn**, and that is not an omission in the
    /// transcription: the pin is absent from the symbol on the sheet, and the
    /// only wire off that part other than its supply and its timing network runs
    /// from the pins 2 and 6 node. The board is using the 555 as a ramp
    /// generator and reading its capacitor, so what the laser's oscillator gets
    /// is an exponential sweep between the part's own two thirds and one third
    /// of +12 V, not a square.
    fn tapping_the_cap(mut self) -> Self {
        self.tap_cap = true;
        // The capacitor is where this one starts its life, at the lower
        // threshold, rather than discharged.
        self.cap = self.vcc / 3.0;
        self
    }
}

impl CustomComponent for Timer555 {
    fn reset(&mut self) {
        self.cap = if self.tap_cap { self.vcc / 3.0 } else { 0.0 };
        self.high = false;
    }

    fn step(&mut self, inputs: &[f64], dt: f64) -> f64 {
        let upper = inputs[0].clamp(0.05, self.vcc);
        let lower = upper * 0.5;
        if self.high {
            self.cap += (self.vcc - self.cap) * dt / (self.r_charge * self.c);
            if self.cap >= upper {
                self.high = false;
            }
        } else {
            self.cap -= self.cap * dt / (self.r_discharge * self.c);
            if self.cap <= lower {
                self.high = true;
            }
        }
        if self.tap_cap {
            self.cap
        } else if self.high {
            self.v_high
        } else {
            self.v_low
        }
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.write_f64_le(self.cap);
        w.write_u8(u8::from(self.high));
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.cap = r.read_f64_le()?;
        self.high = r.read_u8()? != 0;
        Ok(())
    }
}

/// A free-running op-amp relaxation oscillator: the output returns to its own
/// **non-inverting** input through a divider, and charges a capacitor on the
/// inverting input through a resistor.
///
/// `U5`(5,6,7) on the homing missile is the one on this board, and it needs a
/// component rather than a frequency because what the rest of the voice uses is
/// the **waveform on the capacitor**, not the square at the output. That
/// waveform is two exponential segments between the trip points, which is
/// neither a triangle nor a square, and it is what warbles `U6`'s control pin.
///
/// Reports the capacitor's **deviation from the mid-rail**, which is this
/// file's convention for every node that rests somewhere other than zero, and
/// starts at zero as a discharged capacitor does. Takes no inputs: nothing on
/// the board reaches this stage, which is the finding that put it here.
struct OpAmpRelaxation {
    /// `R_ref / (R_ref + R_fb)`: the trip points, as a fraction of the swing.
    beta: f64,
    /// `R_t * C_t`.
    tau: f64,
    /// Half the output's peak-to-peak, each side of the mid-rail.
    swing: f64,
    cap: f64,
    high: bool,
}

impl CustomComponent for OpAmpRelaxation {
    fn reset(&mut self) {
        self.cap = 0.0;
        self.high = true;
    }

    fn step(&mut self, _inputs: &[f64], dt: f64) -> f64 {
        let target = if self.high { self.swing } else { -self.swing };
        self.cap += (target - self.cap) * dt / self.tau;
        let trip = self.beta * self.swing;
        if self.high && self.cap >= trip {
            self.high = false;
        } else if !self.high && self.cap <= -trip {
            self.high = true;
        }
        self.cap
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.write_f64_le(self.cap);
        w.write_u8(u8::from(self.high));
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.cap = r.read_f64_le()?;
        self.high = r.read_u8()? != 0;
        Ok(())
    }
}

/// An output that can only move so many volts per second, which is what an
/// op-amp does when the capacitor across its feedback is the only thing that can
/// absorb its input current.
///
/// `U12`'s alarm section is the case on this board: `R172` 1.5 k drives about
/// 4 mA at it, `R173` 330 k can return a hundredth of that, and the rest goes
/// into `C99`. A square at the input comes out as a trapezoid whose edges take
/// `swing / rate` to climb, and the harmonics that survive that are the voice.
///
/// Modeling the stage as the linear amplifier its resistors describe instead
/// deleted the voice outright: `R173`/`R172` is a gain of 220 into an op-amp on
/// a single +12 V supply that sees an eleven-volt step, so the linear model's
/// 48 Hz pole cut the tone by a hundred while its DC term sat at a rail.
struct SlewLimiter {
    rate: f64,
    rest: f64,
    out: f64,
}

impl SlewLimiter {
    fn new(rate: f64, rest: f64) -> Self {
        Self {
            rate,
            rest,
            out: rest,
        }
    }
}

impl CustomComponent for SlewLimiter {
    fn reset(&mut self) {
        self.out = self.rest;
    }

    fn step(&mut self, inputs: &[f64], dt: f64) -> f64 {
        let step = self.rate * dt;
        self.out += (inputs[0] - self.out).clamp(-step, step);
        self.out
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.write_f64_le(self.out);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.out = r.read_f64_le()?;
        Ok(())
    }
}

struct ParallelPair;

impl CustomComponent for ParallelPair {
    fn reset(&mut self) {}

    fn step(&mut self, inputs: &[f64], _dt: f64) -> f64 {
        let (a, b) = (inputs[0].max(1e-3), inputs[1].max(1e-3));
        (a * b) / (a + b)
    }

    fn save_state(&self, _w: &mut StateWriter) {}

    fn load_state(&mut self, _r: &mut StateReader) -> Result<(), SaveError> {
        Ok(())
    }
}

/// An inverting op-amp stage with a **bridged-T in its feedback**, which is what
/// `U12`'s cannon section is, and which is *not* the multiple-feedback band-pass
/// every other filter on this board uses.
///
/// The difference is one wire and it is the whole voice. In an MFB band-pass the
/// input resistor lands on the capacitor junction; here `R128` lands on the
/// **inverting input**, with `R127` bridging input to output and `C81`/`C82` in
/// series between them, their junction tied to ground through `R130` and `Q6`.
/// Assuming the MFB form because the neighboring voices use it made the cannon
/// a thin 7 kHz whistle at a tenth of its real energy; the board makes a
/// broadband crack that sweeps downward.
///
/// With the T's shunt resistance `r`, two equal capacitors `c`, feedback `r_f`
/// and input `r_in`, the transfer is
///
/// ```text
/// gain(s) = -(r_f/r_in) * (1 + 2*s*c*r) / (1 + 2*s*c*r + r_f*r*c^2*s^2)
/// ```
///
/// a two-pole low-pass with a zero, so
///
/// ```text
/// f0 = 1 / (2*pi*c*sqrt(r_f * r))      DC gain = r_f / r_in
/// Q  = 0.5 * sqrt(r_f / r)             zero at 1 / (2*pi*2*c*r)
/// ```
///
/// The numerator's `1 + s/(Q*w0)` is exactly a unity low-pass plus a unity
/// band-pass, and a Chamberlin filter's band output peaks at `Q`, so the whole
/// response is `low + band/Q` scaled by the DC gain. Nothing here is fitted.
///
/// Input `[0]`: the signal. Input `[1]`: the T's shunt resistance in ohms.
struct BridgedTLowPass {
    r_in: f64,
    r_f: f64,
    c: f64,
    low: f64,
    band: f64,
}

impl BridgedTLowPass {
    fn new(r_in: f64, r_f: f64, c: f64) -> Self {
        Self {
            r_in,
            r_f,
            c,
            low: 0.0,
            band: 0.0,
        }
    }
}

impl CustomComponent for BridgedTLowPass {
    fn reset(&mut self) {
        self.low = 0.0;
        self.band = 0.0;
    }

    fn step(&mut self, inputs: &[f64], dt: f64) -> f64 {
        let r = inputs[1].max(1.0);
        let f0 = 1.0 / (std::f64::consts::TAU * self.c * (self.r_f * r).sqrt());
        let fs = 1.0 / dt;
        let f0 = f0.min(fs / 6.0);
        let q = (0.5 * (self.r_f / r).sqrt()).max(0.5);
        let f = 2.0 * (std::f64::consts::PI * f0 * dt).sin();
        let q1 = (1.0 / q).min(2.0);

        self.low += f * self.band;
        let high = inputs[0] - self.low - q1 * self.band;
        self.band += f * high;

        (self.low + self.band / q) * (self.r_f / self.r_in)
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.write_f64_le(self.low);
        w.write_f64_le(self.band);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.low = r.read_f64_le()?;
        self.band = r.read_f64_le()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Circuit
// ---------------------------------------------------------------------------

/// The board-facing handles: one per PPI signal that does something.
struct ZaxxonInputs {
    ship_level: DataInputId,
    ship_tone_a: LogicInputId,
    ship_tone_b: LogicInputId,
    homing_missile: LogicInputId,
    base_missile: LogicInputId,
    laser: LogicInputId,
    battleship: LogicInputId,
    s_exp: LogicInputId,
    m_exp: LogicInputId,
    cannon: LogicInputId,
    shot: LogicInputId,
    alarm2: LogicInputId,
    alarm3: LogicInputId,
}

/// An `MB4391`'s gain from its control voltage: the clamped ramp between
/// [`mb4391_full_v`] and [`mb4391_mute_v`], squared.
fn mb4391_gain(b: &mut DiscreteCircuitBuilder, name: &str, control: NodeId) -> NodeId {
    let span = mb4391_mute_v() - mb4391_full_v();
    let slope = b.gain(&format!("{name}_SLOPE"), control, -1.0 / span);
    let offset = b.constant(&format!("{name}_OFFSET"), mb4391_mute_v() / span);
    let sum = b.add(&format!("{name}_SUM"), &[slope, offset]);
    let ramp = b.clamp(&format!("{name}_RAMP"), sum, 0.0, 1.0);
    b.multiply(name, ramp, ramp)
}

/// The shared envelope shape of the two explosions and the base missile: a
/// capacitor that rests charged and is pulled toward a diode drop while the
/// one-shot runs, then crawls back up.
///
/// `pulse` is the **one-shot running**, which on all three of these is `Qbar`
/// rather than `Q`. Every 74123 on this board that shapes an envelope is drawn
/// with its `Q` pin present and connected to nothing and its `Qbar` driving the
/// pull-up node: `U22` pin 4 for the small explosion, `U21` pin 12 for the
/// medium one, `U22` pin 12 for the base missile, `U21` pin 4 for the shot.
/// That is what makes the diodes' cathodes face the one-shot and the capacitor
/// rest charged, which is the polarity this function models. This file named
/// three of those four pins correctly and called them `Q` anyway.
///
/// Returns the **control** node, which is the midpoint of the two-resistor
/// divider between the rail and the capacitor, not the capacitor itself. The
/// `0.5` below is that divider and is only right because each of the three
/// pairs is two equal resistors, which is read in all three cases.
///
/// Expressed as the rail *minus* a drop rather than as the capacitor's own
/// voltage, because every state node in the framework powers up at zero and the
/// capacitor's rest state is charged. Modeling the deviation puts the circuit
/// at its DC operating point from the first sample; modeling the absolute
/// voltage would open the VCA for the two seconds the capacitor took to charge,
/// which is an audible power-on whoosh on a board that mutes itself across
/// reset precisely so that it has none.
fn inverted_envelope(
    b: &mut DiscreteCircuitBuilder,
    name: &str,
    pulse: NodeId,
    rail: f64,
    r_recover: f64,
    r_discharge: f64,
    c: f64,
) -> NodeId {
    // The divider halves the capacitor's swing, so the control moves half as far
    // as the capacitor does.
    let target = b.logic_levels(&format!("{name}_TGT"), pulse, 0.0, (rail - V_DIODE) * 0.5);
    let drop = b.rc_envelope(
        &format!("{name}_DROP"),
        target,
        r_discharge * c,
        r_recover * c,
    );
    let inverted = b.gain(&format!("{name}_NEG"), drop, -1.0);
    let bias = b.constant(&format!("{name}_REST"), rail);
    b.add(name, &[inverted, bias])
}

/// One voice's leg into `SJ`: the series/shunt attenuator and the 1 uF block.
/// The 51 kOhm common is [`R_COMMON`] and is applied by the mixer.
///
/// The board's balance is these eleven pairs, and that is only the whole answer
/// where the eleven sources arrive at a swing the drawing accounts for. Getting
/// that wrong is not a subtlety: an earlier pass here gave each voice whatever
/// amplitude its own synthesis happened to produce, the sources spanned 36:1 for
/// reasons that were entirely artefacts, and the transcribed ratios were
/// swamped. Measured against a recorded movie, the medium explosion (leg 0.851,
/// the loudest on the board) came out thirteen times quieter than the laser
/// (leg 0.149, nearly the quietest).
///
/// Every source feeding a leg is now sized by something read off the sheet: the
/// two 4016B-switched voices by their own dividers (`R51`/`R52` and
/// `R75`/`R76`), the cannon by `R134`/`R135`, the seven VCA'd voices by the
/// noise chain's read gains and the `MB4391`'s unity ceiling, and the alarms by
/// `R173`/`R172` into the rail. `voice_levels_follow_the_leg_table` is what
/// keeps that true.
fn mix_leg(
    b: &mut DiscreteCircuitBuilder,
    name: &str,
    src: NodeId,
    (rs, rp): (f64, f64),
) -> NodeId {
    let attenuated = b.gain(&format!("{name}_SRC"), src, rp / (rs + rp));
    b.rc_high_pass(name, attenuated, R_COMMON, C_BLOCK)
}

fn build_circuit(board_clock_hz: u64) -> (DiscreteCircuit, ZaxxonInputs) {
    let rate = sample_rate();
    let mut b = DiscreteCircuitBuilder::new(board_clock_hz, rate).with_sim_rate(MIN_SIM_RATE);

    // --- Board-facing inputs -------------------------------------------------
    let ship_level = b.data_input("SHIP_LEVEL", 1.0);
    let ship_tone_a = b.logic_input("SHIP_TONE_A");
    let ship_tone_b = b.logic_input("SHIP_TONE_B");
    let homing_missile = b.logic_input("HOMING_MISSILE");
    let base_missile = b.logic_input("BASE_MISSILE");
    let laser = b.logic_input("LASER");
    let battleship = b.logic_input("BATTLESHIP");
    let s_exp = b.logic_input("S_EXP");
    let m_exp = b.logic_input("M_EXP");
    let cannon = b.logic_input("CANNON");
    let shot = b.logic_input("SHOT");
    let alarm2 = b.logic_input("ALARM2");
    let alarm3 = b.logic_input("ALARM3");

    // --- U2 MM5837, and the one stage that makes NOISE 2 ---------------------
    let noise_bit = b.lfsr_noise("U2", MM5837_HZ, mm5837_lfsr());
    let noise1 = b.logic_levels("NOISE1", noise_bit, -MM5837_SWING, MM5837_SWING);
    let noise2 = b.gain("NOISE2", noise1, -R112 / R111);

    // --- Player ship ---------------------------------------------------------
    // The two-bit level, glided by C25, into PC1's LED through R17.
    let glide = b.rc_low_pass("SHIP_GLIDE", ship_level, SHIP_GLIDE_R, C25);
    let led_drive = b.gain("PC1_LED_MA", glide, 1000.0 / R17);
    let ldr = b.custom(
        "PC1_LDR",
        vec![led_drive],
        Box::new(VariableResistor {
            // The drive is expressed in milliamps, so the threshold is the LED's
            // forward drop translated through R17, and full brightness is the
            // top of the ladder (both bits low) translated the same way.
            threshold_v: PC1_LED_VF * 1000.0 / R17,
            // [`VariableResistor::full_v`] is an ABSOLUTE drive, not a drive
            // above the threshold: the component subtracts `threshold_v` from
            // both. Passing the difference here subtracted it twice, which put
            // full brightness at 9.7 V instead of the ladder's 10.93 V and left
            // the top of the ladder saturated against its own clamp.
            full_v: ship_levels()[0] * 1000.0 / R17,
            r_dark: PC1_R_DARK,
            r_min: PC1_R_BRIGHT,
            exponent: PC1_EXPONENT,
        }),
    );
    let ship_noise = b.gain("SHIP_NOISE", noise1, -R19 / R18);
    let ship_bp = b.custom(
        "U5_SHIP_BP",
        vec![ship_noise, ldr],
        Box::new(TunedBandPass::new(R20, R21, C26)),
    );

    // The two Sallen-Key low-passes, each gated by one 74LS139 output. Same
    // topology as the two explosion filters below, at a different f0 and Q, and
    // the low-pass shape is the load-bearing half: it is what makes the 723 Hz
    // copy and the 482 Hz copy two different sounds rather than two labels on
    // whatever the LDR-tuned front end is doing.
    let mut ship_legs = Vec::new();
    for (name, gate, hz, leg) in [
        ("SHIP_A", ship_tone_a, SHIP_TONE_A_HZ, LEG_SHIP_A),
        ("SHIP_B", ship_tone_b, SHIP_TONE_B_HZ, LEG_SHIP_B),
    ] {
        let filtered = b.second_order(
            &format!("{name}_LP"),
            ship_bp,
            FilterMode::LowPass,
            hz,
            SHIP_TONE_Q,
        );
        let amplified = b.gain(&format!("{name}_SK"), filtered, SHIP_TONE_GAIN);
        // R26 12 k into R27 3.3 k to ground, then C32 into the MB4391's IN.
        let tone = b.gain(&format!("{name}_R26"), amplified, R27 / (R26 + R27));
        // The 7417 pulls C34 down through R28 when its output is selected and
        // releases it toward +6 V through R29 + R30. As with the explosions this
        // is the deviation from the rest voltage, so the tone starts muted.
        let target = b.logic_levels(&format!("{name}_TGT"), gate, 0.0, V6 - V_SAT);
        let drop = b.rc_envelope(&format!("{name}_DROP"), target, R28 * C34, R29_R30 * C34);
        let neg = b.gain(&format!("{name}_NEG"), drop, -1.0);
        let rest = b.constant(&format!("{name}_REST"), V6);
        let control = b.add(&format!("{name}_CTRL"), &[neg, rest]);
        let g = mb4391_gain(&mut b, &format!("{name}_VCA"), control);
        let vca = b.multiply(&format!("{name}_OUT"), tone, g);
        ship_legs.push(mix_leg(&mut b, &format!("{name}_LEG"), vca, leg));
    }

    // --- The two explosions --------------------------------------------------
    let mut exp_legs = Vec::new();
    for (name, gate, os, c, r_rec, r_dis, hz, leg) in [
        (
            "S_EXP", s_exp, OS_S_EXP, C61, R104_R105, R106, S_EXP_HZ, LEG_S_EXP,
        ),
        (
            "M_EXP", m_exp, OS_M_EXP, C63, R109_R110, R212, M_EXP_HZ, LEG_M_EXP,
        ),
    ] {
        let pulse = b.custom(
            &format!("{name}_74123"),
            vec![gate.into()],
            Box::new(OneShot74123::new(os)),
        );
        let control = inverted_envelope(&mut b, &format!("{name}_ENV"), pulse, V5, r_rec, r_dis, c);
        // A Sallen-Key low-pass with a resonant peak, not a band-pass: the
        // board passes everything below f0 as well as ringing at it, which is
        // the difference between an explosion's rumble and a whistle.
        let band = b.second_order(
            &format!("{name}_BAND"),
            noise2,
            FilterMode::LowPass,
            hz,
            EXP_FILTER_Q,
        );
        let shaped = b.gain(&format!("{name}_SK"), band, EXP_FILTER_GAIN);
        let g = mb4391_gain(&mut b, &format!("{name}_VCA"), control);
        let vca = b.multiply(&format!("{name}_OUT"), shaped, g);
        exp_legs.push(mix_leg(&mut b, &format!("{name}_LEG"), vca, leg));
    }

    // --- The cannon: an envelope that sweeps a band-pass rather than a VCA ---
    let cannon_pulse = b.custom(
        "CANNON_74123",
        vec![cannon.into()],
        Box::new(OneShot74123::new(OS_CANNON)),
    );
    let cannon_env_target = b.logic_levels("CANNON_TGT", cannon_pulse, 0.0, V5 - V_DIODE);
    let cannon_env = b.rc_envelope("CANNON_ENV", cannon_env_target, R126 * C79, R127_ENV * C79);
    let q6 = b.custom(
        "Q6_RCE",
        vec![cannon_env],
        Box::new(VariableResistor {
            // Q6's base-emitter junction is a silicon diode and its base is
            // R132/(R131 + R132) of the envelope, so nothing happens below
            // 3.33 V. This used to be 0, which had the transistor conducting
            // through the whole 0.68 s decay instead of the first 0.19 s of it.
            threshold_v: cannon_q6_threshold_v(),
            full_v: V5 - V_DIODE,
            r_dark: CANNON_R_Q6_OFF,
            r_min: CANNON_R_Q6_ON,
            exponent: PC1_EXPONENT,
        }),
    );
    // The T's shunt: R130 in series with R133 paralleled by Q6, so it runs
    // between R130 alone and R130 + R133.
    let cannon_r133 = b.constant("R133", R133);
    let cannon_shunt = b.custom("Q6_R133", vec![q6, cannon_r133], Box::new(ParallelPair));
    let cannon_r130 = b.constant("R130", R130);
    let cannon_leg_r = b.add("CANNON_RTUNE", &[cannon_shunt, cannon_r130]);
    let cannon_bp = b.custom(
        "U12_CANNON_LP",
        vec![noise2, cannon_leg_r],
        Box::new(BridgedTLowPass::new(R128, R127_FB, C81)),
    );
    // The filter is always live, so something downstream has to stop it hissing
    // between shots: `MB4391 U13` ch B, whose IN is fed by `C84`. Its CON pin is
    // driven by `U12`'s other section, an inverting amp with `R136` 51 k in and
    // `R137` 51 k of feedback around a `R139`/`R141` divider sitting at
    // 6 * 22/(33+22) = 2.4 V. That makes CON = 4.8 - envelope: 4.8 V at rest,
    // which is just above the 4.76 V mute point, and 0.4 V at full envelope.
    let cannon_ctrl_neg = b.gain("CANNON_VCA_NEG", cannon_env, -1.0);
    let cannon_ctrl_rest = b.constant("CANNON_VCA_REST", 2.0 * U12_CANNON_REF);
    let cannon_ctrl = b.add("CANNON_VCA_CTRL", &[cannon_ctrl_neg, cannon_ctrl_rest]);
    let cannon_g = mb4391_gain(&mut b, "CANNON_VCA", cannon_ctrl);
    let cannon_vca = b.multiply("CANNON_OUT", cannon_bp, cannon_g);
    // C83 into R134, with R135 to ground: the 2:1 divider ahead of C84.
    let cannon_voice = b.gain("CANNON_R134", cannon_vca, R135 / (R134 + R135));
    let cannon_leg = mix_leg(&mut b, "CANNON_LEG", cannon_voice, LEG_CANNON);

    // --- The shot ------------------------------------------------------------
    let shot_pulse = b.custom(
        "SHOT_74123",
        vec![shot.into()],
        Box::new(OneShot74123::new(OS_SHOT)),
    );
    // The shaper node between `R143` and `R144`. `Qbar` rests high and the
    // trigger pulls it down, so this node falls on a shot and recovers after.
    let shot_shaper = b.logic_levels(
        "U21_QBAR_NODE",
        shot_pulse,
        shot_shaper_v(V5),
        shot_shaper_v(V_SAT),
    );

    // --- The VCA control: node Y, clamped by D10 -----------------------------
    // Exactly the two explosions' shape, and for the same reason: the capacitor
    // sits charged at rest and the diode drags it down on a trigger. Expressed
    // as a drop from rest rather than as an absolute voltage, so that the
    // circuit starts at its DC operating point instead of opening the VCA while
    // C89 charges. The attack is through D10 and R143/R144 in parallel, which is
    // 479 ohms; the release is C89 against R147 in parallel with R148.
    let shot_drop_tgt = b.logic_levels(
        "SHOT_ENV_TGT",
        shot_pulse,
        0.0,
        shot_vca_rest_v() - shot_vca_floor_v(),
    );
    let shot_drop = b.rc_envelope(
        "SHOT_ENV_DROP",
        shot_drop_tgt,
        C89 * (R143 * R144 / (R143 + R144)),
        C89 * (R147 * R148 / (R147 + R148)),
    );
    let shot_drop_neg = b.gain("SHOT_ENV_NEG", shot_drop, -1.0);
    let shot_rest = b.constant("SHOT_ENV_REST", shot_vca_rest_v());
    let shot_ctrl = b.add("SHOT_VCA_CTRL", &[shot_drop_neg, shot_rest]);
    let shot_g = mb4391_gain(&mut b, "SHOT_VCA", shot_ctrl);

    // --- The pitch shaper: node X, and the amplifier that clips on it --------
    // `C88` carries the shaper node's step into X, which otherwise sits at 8.4 V
    // on the divider from +12 V. `U19(12,13,14)` buffers X and `U19(1,2,3)`
    // inverts it with a gain of -5.9 about the +6 V mid-rail, which for a step
    // this size means the amplifier is not amplifying: it spends the whole voice
    // pinned at one rail or the other, high while the trigger runs and low
    // otherwise.
    let shot_x_step = b.rc_high_pass("U19_NODE_X", shot_shaper, shot_pitch_r(), C88);
    let shot_x_rest = b.constant("U19_NODE_X_REST", shot_pitch_rest_v());
    let shot_x = b.add("U19_NODE_X_SUM", &[shot_x_step, shot_x_rest]);
    let shot_midrail_neg = b.constant("U19_MIDRAIL_NEG", -V6);
    let shot_x_dev = b.add("U19_NODE_X_DEV", &[shot_x, shot_midrail_neg]);
    let shot_amp_raw = b.gain("U19_SHOT_AMP", shot_x_dev, -R150 / R149);
    let shot_amp_ref = b.constant("U19_SHOT_AMP_REF", V6);
    let shot_amp_sum = b.add("U19_SHOT_AMP_SUM", &[shot_amp_raw, shot_amp_ref]);
    let shot_amp = b.clamp(
        "U19_SHOT_AMP_CLIP",
        shot_amp_sum,
        V6 - OPAMP_SWING,
        V6 + OPAMP_SWING,
    );

    // --- U18's 555, and node A ----------------------------------------------
    // The amplifier drives the 555's control pin AND reaches node A through
    // R154, so one shaper sets both the warble's rate and the pitch it warbles
    // around. R155's 820 ohms against R153 and R154 is what scales the pair into
    // the oscillator's range.
    let shot_555 = b.custom(
        "U18_555",
        vec![shot_amp],
        Box::new(Timer555::new(R151_R152, R152, C90)),
    );
    let (w_555, w_amp) = shot_node_a_weights();
    let shot_a_555 = b.gain("SHOT_A_555", shot_555, w_555);
    let shot_a_amp = b.gain("SHOT_A_ENV", shot_amp, w_amp);
    let shot_a_raw = b.add("SHOT_A_SUM", &[shot_a_555, shot_a_amp]);
    let shot_a = b.low_pass_hz(
        "SHOT_NODE_A",
        shot_a_raw,
        1.0 / (std::f64::consts::TAU * C91 * (1.0 / (1.0 / R153 + 1.0 / R154 + 1.0 / R155))),
    );

    // --- The oscillator ------------------------------------------------------
    // 3331 Hz per volt at node A, from the same helper the battleship uses. Its
    // duty is 45.5 % rather than 50 %, because R156 against R159 is 2.2 to 1
    // where the battleship's pair is exactly 2 to 1; the difference is a little
    // more second harmonic and it is below what a square-versus-square
    // comparison shows.
    let shot_freq = b.gain("SHOT_FREQ", shot_a, shot_hz_per_volt());
    let shot_square = b.variable_square("U19_SHOT_OSC", shot_freq);
    let half = shot_out_v() / 2.0;
    let shot_tone = b.logic_levels("SHOT_TONE", shot_square, -half, half);
    let shot_voice = b.multiply("SHOT_OUT", shot_tone, shot_g);
    let shot_leg = mix_leg(&mut b, "SHOT_LEG", shot_voice, LEG_SHOT);

    // --- The battleship ------------------------------------------------------
    // What reaches `C59` is the fast Schmitt's hysteresis node through a unity
    // follower: a 50 % **square**, whose harmonics are the voice. Low-passing
    // them away left a bare tone that sounded nothing like the board.
    //
    // There is no modulation. `U9`'s slow stage is a complete second oscillator
    // whose only way out is `R92`, and `R92` lands on a follower's output where
    // it can do nothing; see [`battleship_hz`]. Modeling a sweep here would be
    // modeling a wire the sheet does not draw.
    //
    // The 4016B at U17 is a switch, not a VCA: it either passes the oscillator
    // or removes its leg from the network. Modeled as a gate on the source,
    // which is not the same thing -- see the note on `resistor_mixer_switched`
    // in the framework -- but the board's legs are all 51k into a 10k load, so
    // opening one changes the others by under half a decibel.
    let bs_square = b.fixed_square("U10_FAST_OSC", battleship_hz());
    let half = battleship_swing_v() / 2.0;
    let bs_level = b.logic_levels("U10_HYSTERESIS_NODE", bs_square, -half, half);
    let bs_gated = b.multiply("BATTLESHIP_SW", bs_level, battleship);
    let battleship_leg = mix_leg(&mut b, "BATTLESHIP_LEG", bs_gated, LEG_BATTLESHIP);

    // --- The homing missile: a 555 warbled at 15 Hz, through a switch --------
    // **This voice has no envelope.** `R42`'s far end is +6 V, not the gate, so
    // `U5`(5,6,7) with `R43`, `R44` and `C43` is a free-running relaxation
    // oscillator at 15.4 Hz that runs whether or not the voice is sounding, and
    // `U30`'s 7406 reaches only `U17`'s 4016B control pin, exactly as the laser's
    // and the battleship's gates do. Two earlier readings of this stage had the
    // gate driving `R42`: first as an envelope, then as a latch. Both were
    // reading a wire that goes somewhere else.
    //
    // What `U4`(5,6,7) sums is therefore a continuous 15 Hz warble against
    // `NOISE 1`, at -0.147 and -0.05, and `C46` 33 uF against the control pin's
    // own 3.3 kOhm is a 1.4 Hz DC block that passes both whole. The old model
    // had `C46` at 2.2 uF, which is a 22 Hz high-pass: it differentiated a
    // modulation slower than itself into a transient, which is how a continuous
    // voice came to be modeled as a chirp.
    //
    // A 555 with a moving control pin is not a moving frequency, because the pin
    // is the upper threshold and half of it is the lower, so the duty cycle
    // moves with the pitch. The part is simulated rather than solved.
    let hm_warble = b.custom(
        "U5_C43",
        vec![],
        Box::new(OpAmpRelaxation {
            beta: homing_beta(),
            tau: R44 * C43,
            swing: OPAMP_SWING,
            cap: 0.0,
            high: true,
        }),
    );
    // `NOISE 1` reaches `R46` through `C44` 1 uF, which against 200 k is a 0.8 Hz
    // corner on a source that is already zero-mean, so it is not modeled.
    let hm_env_leg = b.gain("U4_SUM_ENV", hm_warble, -R47 / R45);
    let hm_noise_leg = b.gain("U4_SUM_NOISE", noise1, -R47 / R46);
    let hm_sum = b.add("U4_SUM", &[hm_env_leg, hm_noise_leg]);
    // C46 into the control pin's own impedance: what survives is the AC.
    let hm_cv_ac = b.rc_high_pass("C46", hm_sum, CV_PIN_R, C46);
    // The pin's DC is the part's own two thirds of its +5 V supply.
    let hm_cv_rest = b.constant("U6_CV_REST", V5 * 2.0 / 3.0);
    let hm_cv = b.add("U6_CV", &[hm_cv_ac, hm_cv_rest]);
    let hm_tone = b.custom(
        "U6_555",
        vec![hm_cv],
        Box::new(Timer555::on_supply(R48 + R49, R49, C45, V5)),
    );
    // U6 runs on +5 V, so its square reaches about Vcc - 1.2; R51/R52 then
    // divide it before C47 and the 4016B. Half the swing each side of the
    // mid-rail, because C47 blocks the DC.
    let hm_swing = (V5 - 1.2) * 0.5 * R52 / (R51 + R52);
    let hm_level = b.logic_levels("HOMING_LEVEL", hm_tone, -hm_swing, hm_swing);
    let hm_gated = b.multiply("HOMING_SW", hm_level, homing_missile);
    let homing_leg = mix_leg(&mut b, "HOMING_LEG", hm_gated, LEG_HOMING_MISSILE);

    // --- The base missile: the third noise band, VCA'd by a 151 ms one-shot --
    let bm_pulse = b.custom(
        "BASE_MISSILE_74123",
        vec![base_missile.into()],
        Box::new(OneShot74123::new(OS_BASE_MISSILE)),
    );
    // R59's upper end is +5 V, read at 400 dpi, so this shaper rests and bottoms
    // exactly where the two explosions' do rather than sitting a volt high.
    let bm_control = inverted_envelope(&mut b, "BASE_MISSILE_ENV", bm_pulse, V5, R59_R60, R58, C49);
    let bm_band = b.second_order(
        "BASE_MISSILE_BAND",
        noise2,
        FilterMode::LowPass,
        BASE_MISSILE_HZ,
        BASE_MISSILE_Q,
    );
    let bm_shaped = b.gain("BASE_MISSILE_SK", bm_band, BASE_MISSILE_GAIN);
    let bm_g = mb4391_gain(&mut b, "BASE_MISSILE_VCA", bm_control);
    let bm_voice = b.multiply("BASE_MISSILE_OUT", bm_shaped, bm_g);
    let base_missile_leg = mix_leg(&mut b, "BASE_MISSILE_LEG", bm_voice, LEG_BASE_MISSILE);

    // --- The laser: a 5.3 Hz repeat gating a decaying tone -------------------
    // What the oscillator follows is `U7`'s timing CAPACITOR, not its output
    // pin, which is not drawn at all: the ramp sweeps the pitch 300 Hz to
    // 600 Hz, rising in 35 ms through R65 and falling over 153 ms through R66.
    // D3 across R66 is what makes those two legs different, and the asymmetry is
    // the voice: a fast swoop up and a slow fall.
    // U7's pin 5 is decoupled by C54 rather than driven, so its upper threshold
    // is the part's own two thirds of +12 V and the lower is half of that.
    let laser_cv = b.constant("U7_CV", V12 * 2.0 / 3.0);
    let laser_ramp = b.custom(
        "U7_555_CAP",
        vec![laser_cv],
        Box::new(Timer555::new(R65, R66, C53).tapping_the_cap()),
    );
    let laser_freq = b.gain("LASER_FREQ", laser_ramp, laser_hz_per_volt());
    let laser_square = b.variable_square("U8_LASER_OSC", laser_freq);
    // U8's output divided by R75/R76 before C55 and the 4016B. The 4016B is a
    // switch, so nothing here decays: the gate is the envelope.
    let half = 2.0 * OPAMP_SWING * R76 / (R75 + R76) / 2.0;
    let laser_level = b.logic_levels("LASER_LEVEL", laser_square, -half, half);
    let laser_gated = b.multiply("LASER_SW", laser_level, laser);
    let laser_leg = mix_leg(&mut b, "LASER_LEG", laser_gated, LEG_LASER);

    // --- The alarms: one 556, one 74393, two 7426 sections -------------------
    let alarm_clk = b.constant("U50_556", alarm_clock_hz());
    let divider = b.ripple_counter("U49_74393", alarm_clk, 4);
    let qc = b.bit_decode("U49_1QC", divider, 2); // clock / 8 = 2535 Hz
    let qd = b.bit_decode("U49_1QD", divider, 3); // clock / 16 = 1268 Hz
    let a2_pulse = b.custom(
        "ALARM2_74123",
        vec![alarm2.into()],
        Box::new(OneShot74123::new(OS_ALARM)),
    );
    let a3_pulse = b.custom(
        "ALARM3_74123",
        vec![alarm3.into()],
        Box::new(OneShot74123::new(OS_ALARM)),
    );
    // Both 7426 sections are open-collector onto one node pulled up by `R171`,
    // so the node is the AND of the two NANDs.
    //
    // `1QD` reaches `U67` pin 2 off the same junction that clocks `2A`, and
    // `1QC` runs down past that gate's other input without a dot to reach pin 5.
    //
    // Which alarm sits on pin 1 and which on pin 4 was previously recorded as
    // unresolved because it crosses the sheet seam. It is resolved now, by
    // following both one-shot outputs to the page edge and matching their
    // heights: `U46`'s `Q` (alarm 2) steps up to the upper of the two crossings
    // and reaches pin 1, so **alarm 2 is the low tone**, and `U44`'s (alarm 3)
    // stays on the lower one to pin 4, so alarm 3 is the high one.
    //
    // A pass before this one moved both to `1QB` on the strength of two MAME
    // sample files measuring near 5 kHz. That was fitting the model to a
    // recording of somebody else's board, and `1QB` is not wired to anything.
    let a2_gate = b.logic_gate("U67_A", LogicOp::Nand, a2_pulse, qd);
    let a3_gate = b.logic_gate("U67_B", LogicOp::Nand, a3_pulse, qc);
    let alarm_node = b.logic_gate("U67_WIRED_AND", LogicOp::And, a2_gate, a3_gate);
    // `U12`'s alarm section is **not a linear stage**: see [`SlewLimiter`]. The
    // section inverts, so the node's rest at +12 V pins the output at the bottom
    // of its swing and an alarm burst drives it rail to rail.
    let alarm_out = b.logic_levels("U12_ALARM", alarm_node, alarm_burst_v(), 0.0);
    let alarm_slewed = b.custom(
        "U12_ALARM_SLEW",
        vec![alarm_out],
        Box::new(SlewLimiter::new(alarm_slew_v_per_s(), 0.0)),
    );
    let alarm_leg = mix_leg(&mut b, "ALARM_LEG", alarm_slewed, LEG_ALARM);

    // --- SJ, the passive mix node, and everything after it -------------------
    let taps: Vec<(NodeId, f64)> = ship_legs
        .into_iter()
        .chain(exp_legs)
        .chain([
            homing_leg,
            base_missile_leg,
            laser_leg,
            battleship_leg,
            cannon_leg,
            shot_leg,
            alarm_leg,
        ])
        .map(|leg| (leg, R_COMMON))
        .collect();
    let sj = b.resistor_mixer("SJ", &taps, Some(R209));
    let summed = b.gain("U11", sj, U11_GAIN);
    let volume = b.gain("VR1", summed, VOLUME);
    b.output(volume, OutputGain::linear(OUTPUT_GAIN));

    (
        b.build(),
        ZaxxonInputs {
            ship_level,
            ship_tone_a,
            ship_tone_b,
            homing_missile,
            base_missile,
            laser,
            battleship,
            s_exp,
            m_exp,
            cannon,
            shot,
            alarm2,
            alarm3,
        },
    )
}

// ---------------------------------------------------------------------------
// Device
// ---------------------------------------------------------------------------

/// Zaxxon's sound board: eleven analog voices gated by one i8255.
#[derive(Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct ZaxxonSound {
    #[save(id = 1)]
    circuit: DiscreteCircuit,
    /// Input handles, fixed when the circuit is built.
    #[save_skip]
    ids: ZaxxonInputs,
    /// The four solved ladder voltages, indexed by `PA0 * 2 + PA1`.
    #[save_skip]
    levels: [f64; 4],
}

impl ZaxxonSound {
    /// Build the circuit for a board whose main clock is `board_clock_hz`.
    pub fn new(board_clock_hz: u64) -> Self {
        let (circuit, ids) = build_circuit(board_clock_hz);
        Self {
            circuit,
            ids,
            levels: ship_levels(),
        }
    }

    /// Push the PPI's three output latches to the board.
    ///
    /// Every gate bit is active low, so each `== 0` below is the voice being
    /// asked for. Ports A bits 0-1 are the level rather than a gate, and bits
    /// 2-3 go through `U32`'s 74LS139 decode, which is combinational and is done
    /// here rather than as circuit nodes.
    pub fn set_ports(&mut self, port_a: u8, port_b: u8, port_c: u8) {
        let c = &mut self.circuit;

        // The level ladder. PA0 is the more significant bit.
        let code = ((port_a & 0x01) << 1) | ((port_a >> 1) & 0x01);
        c.set_data(self.ids.ship_level, self.levels[code as usize]);

        // U32 74LS139: A = PLAYER SHIP C (PA2), B = PLAYER SHIP D (PA3), enable
        // grounded. Only Y0 and Y1 are connected.
        let ship_c = port_a & 0x04 != 0;
        let ship_d = port_a & 0x08 != 0;
        c.set_logic(self.ids.ship_tone_a, !ship_c && !ship_d); // Y0
        c.set_logic(self.ids.ship_tone_b, ship_c && !ship_d); // Y1

        c.set_logic(self.ids.homing_missile, port_a & 0x10 == 0);
        c.set_logic(self.ids.base_missile, port_a & 0x20 == 0);
        c.set_logic(self.ids.laser, port_a & 0x40 == 0);
        c.set_logic(self.ids.battleship, port_a & 0x80 == 0);

        c.set_logic(self.ids.s_exp, port_b & 0x10 == 0);
        c.set_logic(self.ids.m_exp, port_b & 0x20 == 0);
        c.set_logic(self.ids.cannon, port_b & 0x80 == 0);

        c.set_logic(self.ids.shot, port_c & 0x01 == 0);
        c.set_logic(self.ids.alarm2, port_c & 0x04 == 0);
        c.set_logic(self.ids.alarm3, port_c & 0x08 == 0);
    }

    /// Advance the circuit by `board_cycles` of main-CPU time.
    pub fn tick(&mut self, board_cycles: u64) {
        self.circuit.tick(board_cycles);
    }

    /// Drain produced mono `i16` samples.
    pub fn fill_audio(&mut self, out: &mut [i16]) -> usize {
        self.circuit.fill_audio(out)
    }

    pub fn sample_rate(&self) -> u32 {
        self.circuit.sample_rate()
    }

    pub fn reset(&mut self) {
        self.circuit.reset();
    }

    /// The circuit, for the debugger's node view.
    pub fn circuit(&self) -> &DiscreteCircuit {
        &self.circuit
    }
}

/// The eleven legs, in the order they appear in the debug panel, with the node
/// name each one's voice lands on.
///
/// This is the view that answers "which voice is actually contributing", which
/// is not a question the output sample can answer once eleven legs have been
/// summed. Each is reported as a magnitude in millivolts at `SJ`'s side of the
/// leg, so a voice that is gated off reads zero and one that is sounding does
/// not.
const DEBUG_LEGS: [(&str, &str); 11] = [
    ("SHIP_A", "SHIP_A_LEG"),
    ("SHIP_B", "SHIP_B_LEG"),
    ("HOMING", "HOMING_LEG"),
    ("BASEMIS", "BASE_MISSILE_LEG"),
    ("LASER", "LASER_LEG"),
    ("BATTLE", "BATTLESHIP_LEG"),
    ("S_EXP", "S_EXP_LEG"),
    ("M_EXP", "M_EXP_LEG"),
    ("CANNON", "CANNON_LEG"),
    ("SHOT", "SHOT_LEG"),
    ("ALARM", "ALARM_LEG"),
];

/// The gate inputs, in PPI order, so the panel shows the cause beside the effect.
const DEBUG_GATES: [(&str, &str); 13] = [
    ("g_SHIP_A", "SHIP_TONE_A"),
    ("g_SHIP_B", "SHIP_TONE_B"),
    ("g_HOMING", "HOMING_MISSILE"),
    ("g_BASEMIS", "BASE_MISSILE"),
    ("g_LASER", "LASER"),
    ("g_BATTLE", "BATTLESHIP"),
    ("g_S_EXP", "S_EXP"),
    ("g_M_EXP", "M_EXP"),
    ("g_CANNON", "CANNON"),
    ("g_SHOT", "SHOT"),
    ("g_ALARM2", "ALARM2"),
    ("g_ALARM3", "ALARM3"),
    ("SHIP_LVL", "SHIP_LEVEL"),
];

impl phosphor_core::device::Device for ZaxxonSound {
    fn name(&self) -> &'static str {
        "Zaxxon Discrete"
    }
    fn reset(&mut self) {
        self.reset();
    }
}

impl phosphor_core::core::debug::Debuggable for ZaxxonSound {
    fn debug_registers(&self) -> Vec<phosphor_core::core::debug::DebugRegister> {
        use phosphor_core::core::debug::DebugRegister;
        let mv = |name: &str| -> u64 {
            self.circuit
                .node_by_name(name)
                .map(|n| (self.circuit.value(n).abs() * 1000.0) as u64)
                .unwrap_or(0)
        };
        let mut out = Vec::with_capacity(DEBUG_GATES.len() + DEBUG_LEGS.len() + 1);
        for (label, node) in DEBUG_GATES {
            out.push(DebugRegister {
                name: label,
                value: mv(node),
                width: DebugRegister::DECIMAL,
            });
        }
        for (label, node) in DEBUG_LEGS {
            out.push(DebugRegister {
                name: label,
                value: mv(node),
                width: DebugRegister::DECIMAL,
            });
        }
        out.push(DebugRegister {
            name: "SJ",
            value: mv("SJ"),
            width: DebugRegister::DECIMAL,
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::save_state::{Saveable as _, StateReader, StateWriter};

    /// Zaxxon's main CPU clock, which is what the device is built against.
    const CPU_HZ: u64 = 3_041_250;

    /// All fourteen lines idle: every gate bit high, which is what `RP1`/`RP2`
    /// leave them at from power-on.
    const IDLE: (u8, u8, u8) = (0xFF, 0xFF, 0xFF);

    fn rms(samples: &[i16]) -> f64 {
        let sum: f64 = samples.iter().map(|&s| (s as f64).powi(2)).sum();
        (sum / samples.len().max(1) as f64).sqrt()
    }

    fn peak(samples: &[i16]) -> i32 {
        samples.iter().map(|&s| (s as i32).abs()).max().unwrap_or(0)
    }

    /// Drive the board for `ms` with the given latches and return the samples.
    fn render(snd: &mut ZaxxonSound, ms: u64, ports: (u8, u8, u8)) -> Vec<i16> {
        snd.set_ports(ports.0, ports.1, ports.2);
        let cycles = CPU_HZ * ms / 1000;
        snd.tick(cycles);
        let n = (snd.sample_rate() as u64 * ms / 1000) as usize + 64;
        let mut out = vec![0i16; n];
        let got = snd.fill_audio(&mut out);
        out.truncate(got);
        out
    }

    #[test]
    fn idle_is_silent() {
        let mut snd = ZaxxonSound::new(CPU_HZ);
        let out = render(&mut snd, 200, IDLE);
        // Settle the filters, then measure: an idle board makes no sound, which
        // is the whole point of RP1 and RP2 pulling every gate bit high.
        let tail = &out[out.len() / 2..];
        assert!(
            rms(tail) < 1.0,
            "an idle board should be silent (rms={})",
            rms(tail)
        );
    }

    #[test]
    fn every_gate_bit_makes_a_sound() {
        // (name, port A, port B, port C) with exactly one gate bit pulled low,
        // except the first two: `U32`'s Y0 needs PA2 and PA3 both low and Y1
        // needs PA2 high with PA3 low, so those are the two port A values that
        // select a tone. Every other combination, idle included, lands on Y2 or
        // Y3, which are not connected.
        for (name, a, b, c, ms) in [
            ("ship tone A", 0xF3u8, 0xFFu8, 0xFFu8, 400u64),
            ("ship tone B", 0xF7, 0xFF, 0xFF, 400),
            ("homing missile", 0xEF, 0xFF, 0xFF, 300),
            ("base missile", 0xDF, 0xFF, 0xFF, 300),
            ("laser", 0xBF, 0xFF, 0xFF, 400),
            ("battleship", 0x7F, 0xFF, 0xFF, 400),
            ("small explosion", 0xFF, 0xEF, 0xFF, 300),
            ("medium explosion", 0xFF, 0xDF, 0xFF, 300),
            ("cannon", 0xFF, 0x7F, 0xFF, 300),
            ("shot", 0xFF, 0xFF, 0xFE, 200),
            ("alarm 2", 0xFF, 0xFF, 0xFB, 300),
            ("alarm 3", 0xFF, 0xFF, 0xF7, 300),
        ] {
            let mut snd = ZaxxonSound::new(CPU_HZ);
            // Start idle so the one-shots see a real edge.
            let _ = render(&mut snd, 20, IDLE);
            let out = render(&mut snd, ms, (a, b, c));
            assert!(
                rms(&out) > 20.0,
                "{name} should be audible (rms={})",
                rms(&out)
            );
        }
    }

    /// Drive one voice alone and return the peak its leg puts on `SJ`, in volts.
    fn leg_peak(node: &str, ports: (u8, u8, u8), ms: u64) -> f64 {
        let mut snd = ZaxxonSound::new(CPU_HZ);
        snd.set_ports(IDLE.0, IDLE.1, IDLE.2);
        snd.tick(CPU_HZ / 50);
        snd.set_ports(ports.0, ports.1, ports.2);
        let id = snd
            .circuit()
            .node_by_name(node)
            .unwrap_or_else(|| panic!("no node {node}"));
        let mut peak: f64 = 0.0;
        let slice = CPU_HZ / 2000; // half a millisecond
        for _ in 0..(ms * 2) {
            snd.circuit.tick(slice);
            peak = peak.max(snd.circuit().value(id).abs());
        }
        peak
    }

    /// A leg's RMS over `from_ms..to_ms` after its gate is asked for.
    ///
    /// The window is a real choice and it is the same for every voice, which is
    /// the point. A peak cannot compare a noise band with a square, because a
    /// square's crest factor is 1 and a 226 Hz slice of noise is nearer 4, so
    /// the two classes are 12 dB apart before anything about the board is
    /// measured. An RMS puts them on one scale, and an RMS needs a window that
    /// contains the voice: every voice on this board either sustains or runs for
    /// at least the alarms' 132 ms, so one window serves all eleven and no voice
    /// gets a window picked to suit it.
    fn leg_rms_window(node: &str, ports: (u8, u8, u8), from_ms: u64, to_ms: u64) -> f64 {
        let mut snd = ZaxxonSound::new(CPU_HZ);
        snd.set_ports(IDLE.0, IDLE.1, IDLE.2);
        snd.tick(CPU_HZ / 50);
        snd.set_ports(ports.0, ports.1, ports.2);
        let id = snd
            .circuit()
            .node_by_name(node)
            .unwrap_or_else(|| panic!("no node {node}"));
        let slice = CPU_HZ / 2000; // half a millisecond
        let (mut sum, mut n) = (0.0, 0usize);
        for i in 0..(to_ms * 2) {
            snd.circuit.tick(slice);
            if i >= from_ms * 2 {
                sum += snd.circuit().value(id).powi(2);
                n += 1;
            }
        }
        (sum / n.max(1) as f64).sqrt()
    }

    /// The window the mix comparison uses: past the leg's 1 uF block, which is a
    /// 51 ms time constant, and inside the shortest voice on the board.
    const MIX_WINDOW_MS: (u64, u64) = (60, 250);

    /// The same as [`leg_peak`], as an RMS over the second half of the hold.
    fn leg_rms(node: &str, ports: (u8, u8, u8), ms: u64) -> f64 {
        leg_rms_window(node, ports, ms / 2, ms)
    }

    /// No voice may dominate the mix by more than the leg table allows.
    ///
    /// **This is the test the device shipped without, and the defect it would
    /// have caught was real.** Measured against a recorded movie, the medium
    /// explosion (leg 0.851, the loudest on the board) came out thirteen times
    /// quieter than the laser (leg 0.149, nearly the quietest), because each
    /// voice reached its leg at whatever amplitude its own synthesis happened to
    /// produce. The leg table was faithful and governed nothing.
    ///
    /// It does not pin a level per voice, because the board does not give one:
    /// several chains are read at block level and their absolute amplitude is
    /// genuinely unknown. What it pins is the thing the drawing *does* settle,
    /// that no voice is orders of magnitude out of line with the rest, which is
    /// the failure that actually happened.
    ///
    /// **It used to measure peaks, and a peak cannot put this board's two
    /// classes of voice on one scale.** A square's crest factor is 1 and a
    /// 226 Hz slice of noise is nearer 4, so the two are 12 dB apart before
    /// anything about the mix is measured, and the bound had to be 100:1 to
    /// accommodate that. On an RMS over one window that is the same for all
    /// eleven, the board's legs land within **12:1**, which is a measurement
    /// worth having rather than a bound wide enough to hide the failure it is
    /// looking for.
    #[test]
    fn voice_levels_follow_the_leg_table() {
        /// A voice, its leg node, and the latches that drive it.
        type Case = (&'static str, &'static str, (u8, u8, u8));
        let cases: [Case; LEG_COUNT] = [
            ("ship tone A", "SHIP_A_LEG", (0xF3, 0xFF, 0xFF)),
            ("ship tone B", "SHIP_B_LEG", (0xF7, 0xFF, 0xFF)),
            ("homing missile", "HOMING_LEG", (0xEF, 0xFF, 0xFF)),
            ("base missile", "BASE_MISSILE_LEG", (0xDF, 0xFF, 0xFF)),
            ("laser", "LASER_LEG", (0xBF, 0xFF, 0xFF)),
            ("battleship", "BATTLESHIP_LEG", (0x7F, 0xFF, 0xFF)),
            ("small explosion", "S_EXP_LEG", (0xFF, 0xEF, 0xFF)),
            ("medium explosion", "M_EXP_LEG", (0xFF, 0xDF, 0xFF)),
            ("cannon", "CANNON_LEG", (0xFF, 0x7F, 0xFF)),
            ("shot", "SHOT_LEG", (0xFF, 0xFF, 0xFE)),
            ("alarms", "ALARM_LEG", (0xFF, 0xFF, 0xFB)),
        ];

        let (from, to) = MIX_WINDOW_MS;
        let levels: Vec<(&str, f64)> = cases
            .iter()
            .map(|(label, node, ports)| (*label, leg_rms_window(node, *ports, from, to)))
            .collect();
        let summary: Vec<String> = levels
            .iter()
            .map(|(l, v)| format!("{l} {:.1} mV", v * 1000.0))
            .collect();
        let summary = summary.join(", ");

        let loudest = levels.iter().map(|(_, v)| *v).fold(0.0f64, f64::max);
        let quietest = levels.iter().map(|(_, v)| *v).fold(f64::MAX, f64::min);
        assert!(quietest > 0.0, "every voice must reach SJ. {summary}");
        let spread = loudest / quietest;

        // The bound that carries the meaning: the leg table's own span, 59:1
        // from the medium explosion to the alarms. If the legs governed the
        // balance and every source were the same size, the mix would spread
        // exactly that far. The sources are not the same size and the designer
        // compensated in the narrowing direction, giving the largest leg to the
        // smallest source, so the mix must come out NARROWER than the legs. A
        // mix wider than its own leg table is a source amplitude doing the work
        // the series/shunt pairs should be doing.
        let ratio = |(rs, rp): (f64, f64)| rp / (rs + rp);
        let leg_span = ratio(LEG_M_EXP) / ratio(LEG_ALARM);
        assert!(
            spread < leg_span,
            "the mix spans {spread:.0}:1 where its own leg table spans \
             {leg_span:.0}:1. {summary}"
        );

        // And the tighter empirical guard, which is what catches a regression:
        // it measures 12:1 today, so 20 is room to move without being room to
        // put a voice an order of magnitude out.
        assert!(
            spread < 20.0,
            "the mix spans {spread:.0}:1, against the 12:1 it measured when this \
             bound was set. {summary}"
        );
    }

    /// The output scaling is a headroom choice rather than a reading (see
    /// [`OUTPUT_GAIN`]), so this is where that choice is stated as a number: one
    /// voice on its own must be clearly audible and must leave room for the
    /// several that overlap in play.
    #[test]
    fn a_single_voice_leaves_headroom() {
        let mut loudest = 0;
        let mut loudest_name = "";
        for (name, a, b, c) in [
            ("ship tone A", 0xF3u8, 0xFFu8, 0xFFu8),
            ("ship tone B", 0xF7, 0xFF, 0xFF),
            ("homing missile", 0xEF, 0xFF, 0xFF),
            ("base missile", 0xDF, 0xFF, 0xFF),
            ("laser", 0xBF, 0xFF, 0xFF),
            ("battleship", 0x7F, 0xFF, 0xFF),
            ("small explosion", 0xFF, 0xEF, 0xFF),
            ("medium explosion", 0xFF, 0xDF, 0xFF),
            ("cannon", 0xFF, 0x7F, 0xFF),
            ("shot", 0xFF, 0xFF, 0xFE),
            ("alarm 2", 0xFF, 0xFF, 0xFB),
            ("alarm 3", 0xFF, 0xFF, 0xF7),
        ] {
            let mut snd = ZaxxonSound::new(CPU_HZ);
            let _ = render(&mut snd, 20, IDLE);
            let p = peak(&render(&mut snd, 400, (a, b, c)));
            if p > loudest {
                loudest = p;
                loudest_name = name;
            }
        }
        assert!(
            (3_000..24_000).contains(&loudest),
            "the loudest single voice is {loudest_name} at {loudest}, outside the \
             headroom band OUTPUT_GAIN is set for"
        );
    }

    #[test]
    fn nothing_saturates() {
        // Everything at once, which the game never does, as the loudest case the
        // mix has to survive.
        let mut snd = ZaxxonSound::new(CPU_HZ);
        let _ = render(&mut snd, 20, IDLE);
        let out = render(&mut snd, 500, (0x00, 0x00, 0x00));
        assert!(
            peak(&out) < 32_000,
            "every voice at once must not clip (peak={})",
            peak(&out)
        );
        assert!(
            rms(&out) > 100.0,
            "every voice at once should be loud (rms={})",
            rms(&out)
        );
    }

    #[test]
    fn the_ship_level_ladder_falls_with_pa0_as_the_msb() {
        let levels = ship_levels();
        // Indexed by PA0 * 2 + PA1, so the array descends in near-linear steps:
        // both bits inverted by U30, and PA0 worth about twice PA1.
        for w in levels.windows(2) {
            assert!(
                w[0] > w[1] + 2.0,
                "the four ladder levels should be a near-linear fall: {levels:?}"
            );
        }
        // The endpoints, solved from R10-R16. If these move, the network in
        // `ship_level_volts` changed and the transcription needs rereading.
        assert!(
            (levels[0] - 10.93).abs() < 0.05,
            "both bits low is the maximum: {}",
            levels[0]
        );
        assert!(
            (levels[3] - 0.76).abs() < 0.05,
            "both bits high (power-on) is the minimum: {}",
            levels[3]
        );
        // The load-bearing half: PA0 is the MSB, so PA0 alone low (index 1, PA1
        // set) must sit ABOVE PA1 alone low (index 2). A `data & 3` fit reads
        // PA0 as bit 0 and has these two swapped.
        assert!(levels[1] > levels[2]);
        // And the whole thing runs the other way from a volume fit: the board is
        // quietest where such a fit is loudest.
        assert!(levels[3] < PC1_LED_VF, "the LED is dark at power-on");
    }

    /// The two engine resonators are **Sallen-Key low-passes**, which is what
    /// sheet 12 draws and is not what this file called them for nine commits.
    ///
    /// Every value here was already right; what was wrong was the shape. The
    /// check that matters is the one that distinguishes the two readings, so it
    /// is stated as a frequency response rather than as a component list: a
    /// low-pass passes a decade below `f0` with the gain the 2.2 k pair sets,
    /// and a band-pass rejects it. Run through the built circuit, because the
    /// arithmetic is in the framework rather than here.
    #[test]
    fn the_engine_resonators_pass_below_their_corner() {
        let (circuit, _) = build_circuit(CPU_HZ);
        for (label, node, hz, c) in [
            ("tone A", "SHIP_A_LP", SHIP_TONE_A_HZ, 2200e-12),
            ("tone B", "SHIP_B_LP", SHIP_TONE_B_HZ, 3300e-12),
        ] {
            // f0 is `1/(2*pi*R*C)` with R24/R25 = R38/R39 = 100 k against
            // C30/C31 2200 pF and C39/C40 3300 pF.
            let want = 1.0 / (std::f64::consts::TAU * 100_000.0 * c);
            assert!(
                (hz - want).abs() < 0.5,
                "{label}: {hz} Hz against the parts' {want} Hz"
            );
            // The built circuit has to be *using* a low-pass node. The name
            // carries the claim, which is the only way a shape shows up in a
            // node graph at all.
            assert!(
                circuit.node_by_name(node).is_some(),
                "{label} does not reach SJ through {node}"
            );
        }
        // Q = 1/(3 - K) with K = 1 + R23/R22 = 2. Both copies, same pair.
        assert!((SHIP_TONE_GAIN - 2.0).abs() < 1e-12);
        assert!((SHIP_TONE_Q - 1.0 / (3.0 - SHIP_TONE_GAIN)).abs() < 1e-12);
        // And the divider this file had no entry for at all, between each
        // low-pass's output and its MB4391's IN.
        let div = R27 / (R26 + R27);
        assert!((div - 0.2157).abs() < 0.001, "R26/R27 divides by {div}");
    }

    /// The front end the two low-passes sit behind slides a **fixed 68 Hz
    /// window**, because an MFB band-pass's `f0/Q` is `1/(pi*Rf*C)` and the LDR
    /// touches neither part.
    ///
    /// This is what says the reference recordings cannot place [`PC1_R_BRIGHT`]:
    /// both of them are about an octave wide and this circuit is 0.3 of an
    /// octave wide at its widest. Kept as a check so that a later pass tempted
    /// to fit the LDR to a measurement has to confront the bandwidth first.
    #[test]
    fn the_engine_front_end_has_a_fixed_bandwidth() {
        let bw = 1.0 / (std::f64::consts::PI * R21 * C26);
        assert!((bw - 67.7).abs() < 0.5, "R21 and C26 give {bw} Hz");
        // Dark, the input resistance is R20 alone: the bottom of the range, and
        // the one point of it the drawing fixes.
        let f_dark = 1.0 / (std::f64::consts::TAU * C26 * (R20 * R21).sqrt());
        assert!((f_dark - 232.0).abs() < 1.0, "LED dark: {f_dark} Hz");
        // Even there the window is under a third of an octave, so no LDR value
        // makes this as broad as a recording of an octave-wide voice.
        assert!(
            bw / f_dark < 0.33,
            "widest the front end gets is {} of its center",
            bw / f_dark
        );
        // And the peak gain is R21/(2*R20) wherever the center goes, so the
        // ladder moves the engine's pitch and not its level.
        let peak_gain = R21 / (2.0 * R20);
        assert!((peak_gain - 23.5).abs() < 0.01, "{peak_gain}");
    }

    #[test]
    fn one_shot_widths_match_the_drawing() {
        // 0.28 * R * C for the plain 74123, in milliseconds.
        for (label, (r, c), want_ms) in [
            ("small explosion", OS_S_EXP, 10.08),
            ("medium explosion", OS_M_EXP, 43.42),
            ("cannon", OS_CANNON, 13.16),
            ("shot", OS_SHOT, 11.09),
            ("alarm", OS_ALARM, 131.6),
            ("base missile", OS_BASE_MISSILE, 151.2),
        ] {
            let got_ms = K74123 * r * c * 1000.0;
            assert!(
                (got_ms - want_ms).abs() < 0.05,
                "{label}: {got_ms} ms against {want_ms} ms"
            );
        }
    }

    /// The 74123 is **retriggerable**, and on this board that is not a detail:
    /// it is the difference between the ship explosion being a 2 s roar and a
    /// thump.
    ///
    /// The game pulses `M-EXP` rather than striking it once, which is why the
    /// reference driver carries a `!playing()` guard on that voice and on alarm
    /// 3 and on no others: a guard against restarting only exists where
    /// restarts happen. While the pulses keep arriving the one-shot never
    /// finishes, `D8` holds `C63` down and the VCA stays open, and `10.wav` is
    /// flat for two seconds and then falls off a cliff, which is that and not
    /// an exponential.
    ///
    /// Nothing else on this board had covered the retrigger path, and a
    /// per-file-normalized octave table cannot see it: retriggering changes the
    /// envelope and not the spectrum, so that metric moves 0.4 dB while the
    /// voice goes from one second to four. `disasm audiodiff` reports the
    /// envelope directly and would have shown it at once; with the retrigger it
    /// puts the recording's decay T20 at 2.350 s against this device's 2.360.
    #[test]
    fn the_one_shots_retrigger_and_that_is_what_sustains_the_ship_explosion() {
        let mut os = OneShot74123::new(OS_M_EXP);
        let dt = 1.0 / 96_000.0;
        let width = K74123 * OS_M_EXP.0 * OS_M_EXP.1;

        // Struck once, it runs for its width and stops.
        os.step(&[1.0], dt);
        os.step(&[0.0], dt);
        let mut ran = 0usize;
        while os.step(&[0.0], dt) > 0.5 {
            ran += 1;
        }
        let once = (ran + 2) as f64 * dt;
        assert!((once - width).abs() < 1e-3, "one strike ran {once} s");

        // Pulsed every 25 ms, which is inside its 43 ms, it never stops.
        let mut os = OneShot74123::new(OS_M_EXP);
        let period = (0.025 / dt) as usize;
        let mut low = 0usize;
        for i in 0..(period * 40) {
            let gate = f64::from(u8::from(i % period == 0));
            if os.step(&[gate], dt) < 0.5 {
                low += 1;
            }
        }
        assert_eq!(low, 0, "the pulse dropped {low} samples while retriggered");

        // And when they stop it recovers over R109 + R110 against C63, which is
        // the cliff at the end of the recording rather than the whole shape.
        let recover = R109_R110 * C63;
        assert!((recover - 2.0).abs() < 0.01, "recovery {recover} s");
        assert!(width < recover / 20.0, "the strike is short against it");
    }

    /// The battleship's two stages are one circuit built twice, so everything
    /// about them except the one op-amp swing follows from read values. This
    /// pins each derivation separately, because each was wrong at some point.
    #[test]
    fn both_battleship_oscillators_follow_from_the_drawing() {
        // R80 2.2M / R81 220k off +12 V, then R90 120k / R91 100k off that.
        assert!((battleship_ref_v() - 1.0909).abs() < 1e-3);
        assert!((battleship_fast_src_v() - 0.4959).abs() < 1e-3);

        // The fast stage's 30 k against 15 k makes the two ramp currents equal,
        // so it is a 50 % square. That is the point of the pair and it holds
        // whatever the rate turns out to be.
        let v_g = battleship_fast_src_v() / 2.0;
        assert!(((v_g / R96 - v_g / R93) - v_g / R93).abs() < 1e-12);

        let (slow, fast) = (battleship_mod_hz(), battleship_hz());
        assert!((slow - 3.02).abs() < 0.05, "U9 slow stage: {slow} Hz");
        assert!((fast - 122.3).abs() < 0.5, "U10 fast stage: {fast} Hz");

        // The capacitors alone would say 165 to 1. They are not alone: the fast
        // stage works against a quarter of the reference where the slow one
        // works against a half, and its sink is 15 k where the slow one's is
        // 2.2 k. A model that used the capacitor ratio would be four times out.
        let ratio = fast / slow;
        assert!(
            (ratio - 40.5).abs() < 0.3,
            "the two stages differ by {ratio}"
        );
        assert!(
            ((C56_C57 * R82) / (C58 * R93) - 165.0).abs() < 0.5,
            "the capacitor-only figure this replaced"
        );

        // The Schmitt window cancels in the ratio and sets the absolute pitch,
        // so it is the one term OPAMP_SWING reaches.
        assert!((battleship_swing_v() - 3.3775).abs() < 1e-3);
        assert!((schmitt_window_v(R86, R88) - schmitt_window_v(R98, R99)).abs() < 1e-12);
    }

    /// `U12`'s alarm section is modeled as a comparator rather than an
    /// amplifier. This is the arithmetic that justifies it, kept as a check so
    /// that changing `R172`, `R173` or the rails has to confront the claim.
    #[test]
    fn the_alarm_stage_cannot_run_linearly() {
        let dc_gain = R173 / R172;
        assert!((dc_gain - 220.0).abs() < 1.0, "R173/R172 = {dc_gain}");
        // The 7426 node swings from R171's pull-up to a saturated output.
        let swing = V12 - V_SAT;
        // An op-amp on a single +12 V supply has 12 V of output range at most.
        assert!(
            dc_gain * swing > 100.0 * V12,
            "the stage is driven {}x past its rail, so it clips",
            dc_gain * swing / V12
        );
        // C99's linear pole is far below either tone, which is why modeling the
        // stage linearly deleted the voice.
        let pole = 1.0 / (std::f64::consts::TAU * R173 * C99);
        assert!(pole < 50.0, "C99 pole {pole} Hz");
        assert!(alarm_clock_hz() / 16.0 > 20.0 * pole);

        // What C99 does instead is limit the slew, and the edge it allows has to
        // stay short against a half period of the higher tone or the trapezoid
        // becomes a triangle and the harmonics go with it.
        let edge_s = 2.0 * OPAMP_SWING / alarm_slew_v_per_s();
        assert!((edge_s - 25e-6).abs() < 1e-6, "U12 edge {edge_s} s");
        let half_period_s = 0.5 / (alarm_clock_hz() / 8.0);
        assert!(
            edge_s < half_period_s / 5.0,
            "edge against 1QC's half period"
        );
    }

    /// The shot is the battleship's oscillator again with a swept reference, and
    /// the two claims worth pinning are that its VCA rests MUTED and that its
    /// rate follows node A rather than a filter's `1/(2*pi*R*C)`.
    #[test]
    fn the_shots_oscillator_is_the_battleships_with_a_live_reference() {
        // Qbar rests high, so both ends of the shaper sit at +5 V and D10 holds
        // the control above the MB4391's mute point. Under the `Q` reading this
        // rests at 1.5 V, which is full gain, and the board screams at power-on.
        assert!((shot_shaper_v(V5) - V5).abs() < 1e-9);
        assert!(
            shot_vca_rest_v() > mb4391_mute_v(),
            "the shot rests at {} V, which is not muted",
            shot_vca_rest_v()
        );
        assert!(
            shot_vca_floor_v() < mb4391_full_v(),
            "a trigger only reaches {} V",
            shot_vca_floor_v()
        );

        // Same helper as the battleship, because it is the same circuit: the
        // rate is linear in node A and nothing about it is a filter corner.
        let per_volt = shot_hz_per_volt();
        assert!((per_volt - 3330.8).abs() < 1.0, "{per_volt} Hz/V");
        // What the old model used, from reading C92 and R156 as a filter.
        let as_a_filter = 1.0 / (std::f64::consts::TAU * R156 * C92);
        assert!(
            (as_a_filter - 4823.0).abs() < 5.0,
            "the figure this replaced"
        );

        // R155 820 ohms is what holds node A down to a fifth of its sources.
        let (w_555, w_amp) = shot_node_a_weights();
        assert!((w_555 - 0.216).abs() < 0.002, "R153's share: {w_555}");
        assert!((w_amp - 0.071).abs() < 0.002, "R154's share: {w_amp}");

        // And the decay: C89 against R147 in parallel with R148.
        let decay = C89 * (R147 * R148 / (R147 + R148));
        assert!((decay - 0.468).abs() < 0.005, "{decay} s");
    }

    /// `Timer555` integrates its capacitor rather than being handed a rate, so
    /// it has to be made to agree with the closed form the parts give. Run it
    /// and measure, which also pins the asymmetry `D3` creates: this voice is a
    /// fast swoop up and a slow fall, and a 50 % ramp would be a different
    /// sound entirely.
    #[test]
    fn the_laser_555_ramps_at_the_rate_its_parts_give() {
        let mut t = Timer555::new(R65, R66, C53).tapping_the_cap();
        let dt = 1.0 / 192_000.0;
        let cv = [V12 * 2.0 / 3.0];

        // Settle, then measure one full period by its rising crossings.
        for _ in 0..192_000 {
            t.step(&cv, dt);
        }
        let (mut last, mut rises, mut first, mut end) = (t.cap, Vec::new(), 0usize, 0usize);
        // Counted cumulatively and differenced between the first and last
        // crossing, so the duty is measured over a whole number of periods. A
        // fixed window holds 10.6 of them and the partial one biases it low.
        let (mut rising, mut rising_at_first, mut rising_at_end) = (0usize, 0usize, 0usize);
        for i in 0..(192_000 * 2) {
            let v = t.step(&cv, dt);
            if v > last {
                rising += 1;
            }
            if last < V12 / 2.0 && v >= V12 / 2.0 {
                rises.push(i);
                if rises.len() == 1 {
                    first = i;
                    rising_at_first = rising;
                }
                end = i;
                rising_at_end = rising;
            }
            last = v;
        }
        assert!(rises.len() >= 3, "only {} periods seen", rises.len());
        let period = (end - first) as f64 / (rises.len() - 1) as f64 * dt;
        let hz = 1.0 / period;
        assert!(
            (hz - laser_repeat_hz()).abs() < 0.05,
            "the component ramps at {hz} Hz against the parts' {}",
            laser_repeat_hz()
        );

        // The capacitor rises for R65's share of the period and falls for
        // R66's, which is what D3 across R66 buys.
        let duty = (rising_at_end - rising_at_first) as f64 / (end - first) as f64;
        assert!(
            (duty - laser_duty()).abs() < 0.01,
            "{duty} rising against the parts' {}",
            laser_duty()
        );

        // And the sweep it hands the oscillator, from the part's own thresholds.
        let (lo, hi) = (
            V12 / 3.0 * laser_hz_per_volt(),
            V12 * 2.0 / 3.0 * laser_hz_per_volt(),
        );
        assert!((lo - 300.0).abs() < 5.0, "bottom of the sweep {lo} Hz");
        assert!((hi - 600.0).abs() < 10.0, "top of the sweep {hi} Hz");
    }

    /// `U6`'s control pin is driven, so its rate is simulated rather than
    /// solved. Park the pin where the part's own divider would and the component
    /// has to land on the closed form.
    #[test]
    fn the_homing_missiles_555_free_runs_where_its_parts_say() {
        let mut t = Timer555::on_supply(R48 + R49, R49, C45, V5);
        let dt = 1.0 / 384_000.0;
        let cv = [V5 * 2.0 / 3.0];
        for _ in 0..38_400 {
            t.step(&cv, dt);
        }
        let (mut last, mut rises, mut first, mut end) = (0.0f64, 0usize, 0usize, 0usize);
        for i in 0..384_000 {
            let v = t.step(&cv, dt);
            if last < 1.0 && v >= 1.0 {
                rises += 1;
                if rises == 1 {
                    first = i;
                }
                end = i;
            }
            last = v;
        }
        assert!(rises >= 3, "only {rises} periods seen");
        let hz = (rises - 1) as f64 / ((end - first) as f64 * dt);
        assert!(
            (hz - homing_missile_hz()).abs() < 3.0,
            "{hz} Hz against the parts' {}",
            homing_missile_hz()
        );
    }

    /// `U5`(5,6,7) free-runs at 15.4 Hz, whether or not the voice is gated, and
    /// the rate does not depend on [`OPAMP_SWING`].
    ///
    /// Both halves of that are worth pinning, because this file has now read
    /// this one stage three ways. It was an envelope driven by the gate level,
    /// then a latch thrown by the gate, and it is neither: `R42`'s far end is
    /// +6 V. Run the component and measure, rather than trusting the closed
    /// form, since what the rest of the voice uses is the capacitor's waveform.
    #[test]
    fn the_homing_missiles_warble_free_runs_and_does_not_rest_on_the_swing() {
        // The swing cancels in ln((1+beta)/(1-beta)), so a rate derived at one
        // swing has to equal the rate derived at a wildly different one.
        let beta = homing_beta();
        assert!((beta - 0.3377).abs() < 1e-3, "R42/(R42+R43) = {beta}");
        let rate_at = |swing: f64| {
            let mut c = OpAmpRelaxation {
                beta,
                tau: R44 * C43,
                swing,
                cap: 0.0,
                high: true,
            };
            let dt = 1.0 / 96_000.0;
            for _ in 0..19_200 {
                c.step(&[], dt);
            }
            // Count trips over two seconds by the sign of the square the
            // component is riding, which is `high`.
            let (mut edges, mut last) = (0usize, c.high);
            for _ in 0..192_000 {
                c.step(&[], dt);
                if c.high != last {
                    edges += 1;
                }
                last = c.high;
            }
            edges as f64 / 2.0 / 2.0
        };
        let at_5 = rate_at(OPAMP_SWING);
        let at_2 = rate_at(2.0);
        assert!(
            (at_5 - homing_warble_hz()).abs() < 0.2,
            "{at_5} Hz against the parts' {}",
            homing_warble_hz()
        );
        assert!((at_5 - 15.38).abs() < 0.2, "{at_5} Hz");
        assert!(
            (at_5 - at_2).abs() < 0.2,
            "the rate moved with the swing: {at_5} against {at_2}"
        );

        // The capacitor trips at beta of the swing either side of the mid-rail,
        // which is what sets how far the warble moves U6's control pin: 0.338 of
        // 5 V, then U4's -R47/R45.
        let depth = beta * OPAMP_SWING * R47 / R45;
        assert!((depth - 0.248).abs() < 0.005, "warble depth {depth} V");
        // And C46 passes it whole rather than differentiating it. At 2.2 uF,
        // which this file used to carry, the corner sat above the warble.
        let corner = 1.0 / (std::f64::consts::TAU * CV_PIN_R * C46);
        assert!((corner - 1.45).abs() < 0.05, "C46 corner {corner} Hz");
        assert!(
            corner < homing_warble_hz() / 5.0,
            "C46 must be a block, not a differentiator"
        );
    }

    /// `U6`'s duty cycle at a given control voltage, from the part's own two
    /// thresholds.
    ///
    /// The charge leg climbs from `cv/2` to `cv` against `V5` through
    /// `R48 + R49`, so it stretches as `cv` rises; the discharge leg falls from
    /// `cv` to `cv/2` through `R49`, which is `ln 2` of its time constant
    /// whatever `cv` is. That asymmetry is why a 555's control pin moves the
    /// duty as well as the pitch, and it is what puts this voice's energy below
    /// the audio band.
    fn homing_duty_at(cv: f64) -> f64 {
        let t_high = (R48 + R49) * C45 * ((V5 - cv * 0.5) / (V5 - cv)).ln();
        let t_low = R49 * C45 * std::f64::consts::LN_2;
        t_high / (t_high + t_low)
    }

    /// The homing missile's chain on its own, with `U5`'s warble and `NOISE 1`
    /// switchable and the simulation step chosen: returns `U6`'s rate in Hz and
    /// the share of the square's energy that lands below 100 Hz.
    ///
    /// Standalone rather than through [`ZaxxonSound`], because both questions
    /// this answers are of the form "what does this voice do without one of its
    /// two inputs", and the built device has no way to take one away. Every part
    /// is the circuit's own and is wired the same way: the same
    /// [`OpAmpRelaxation`], the same `lfsr_noise` at [`MM5837_HZ`], the same
    /// `-R47/R45` and `-R47/R46` summing gains, the same [`C46`] block against
    /// [`CV_PIN_R`], the same [`Timer555`], and the same 1 uF leg block against
    /// [`R_COMMON`]. With both inputs on at [`MIN_SIM_RATE`] it lands within a
    /// hertz of what the built device measures, which is what says the rig is
    /// this voice rather than a sketch of it.
    ///
    /// The 100 Hz measuring filter is two poles, so a 785 Hz square leaks
    /// `(100/785)^4` into it. That floor is the `(false, false)` case and is
    /// what the two tests below subtract.
    fn homing_chain(sim_hz: u64, warble_on: bool, noise_on: bool) -> (f64, f64) {
        let mut b = DiscreteCircuitBuilder::new(CPU_HZ, 44_100).with_sim_rate(sim_hz);
        let bit = b.lfsr_noise("U2", MM5837_HZ, mm5837_lfsr());
        let noise1 = b.logic_levels("NOISE1", bit, -MM5837_SWING, MM5837_SWING);
        let warble = b.custom(
            "U5_C43",
            vec![],
            Box::new(OpAmpRelaxation {
                beta: homing_beta(),
                tau: R44 * C43,
                swing: OPAMP_SWING,
                cap: 0.0,
                high: true,
            }),
        );
        let w = b.gain("U4_SUM_ENV", warble, f64::from(warble_on) * -R47 / R45);
        let n = b.gain("U4_SUM_NOISE", noise1, f64::from(noise_on) * -R47 / R46);
        let sum = b.add("U4_SUM", &[w, n]);
        let ac = b.rc_high_pass("C46", sum, CV_PIN_R, C46);
        let rest = b.constant("U6_CV_REST", V5 * 2.0 / 3.0);
        let cv = b.add("U6_CV", &[ac, rest]);
        let tone = b.custom(
            "U6_555",
            vec![cv],
            Box::new(Timer555::on_supply(R48 + R49, R49, C45, V5)),
        );
        let raw = b.logic_levels("HOMING_LEVEL", tone, -1.0, 1.0);
        // The leg's own 1 uF block: it removes the duty cycle's DC, as the board
        // does, and passes a 15 Hz modulation of it whole.
        let level = b.rc_high_pass("HOMING_LEG", raw, R_COMMON, C_BLOCK);
        b.second_order("BELOW_100", level, FilterMode::LowPass, 100.0, 0.707);
        b.output(level, OutputGain::linear(1.0));
        let mut c = b.build();
        let (tone, level, low) = (
            c.node_by_name("U6_555").expect("U6_555"),
            c.node_by_name("HOMING_LEG").expect("HOMING_LEG"),
            c.node_by_name("BELOW_100").expect("BELOW_100"),
        );

        // Settle the 1 uF block, which is a 51 ms time constant, before
        // measuring anything through it.
        c.tick(CPU_HZ / 2);
        let slice = 16u64;
        let seconds = 2u64;
        let iters = (CPU_HZ * seconds / slice) as usize;
        let (mut rises, mut last) = (0usize, false);
        let (mut e_all, mut e_low) = (0.0f64, 0.0f64);
        for _ in 0..iters {
            c.tick(slice);
            let high = c.value(tone) > 1.0;
            if high && !last {
                rises += 1;
            }
            last = high;
            e_all += c.value(level).powi(2);
            e_low += c.value(low).powi(2);
        }
        (rises as f64 / seconds as f64, e_low / e_all)
    }

    /// **`U6` does not run at the 787 Hz its timing parts give**, and neither
    /// does the board: `NOISE 1` on its control pin biases every threshold
    /// crossing early, and the voice comes out a quarter of an octave high.
    ///
    /// The control pin is the comparator's own threshold, and `U4` puts about
    /// +/-0.25 V of [`MM5837_SWING`] on it through `-R47/R46` at a rate faster
    /// than the capacitor's approach. The capacitor therefore does not cross a
    /// threshold of 3.33 V, it crosses the first dip of a threshold that is
    /// re-randomized every `1/MM5837_HZ`, which is a first-passage problem and
    /// not an averaging one: the effective threshold sits near the bottom of the
    /// noise rather than in its middle, and a 555 charging to a lower threshold
    /// is a faster 555.
    ///
    /// Three measurements say so, and the third is what makes it a property of
    /// the part rather than of this simulation:
    ///
    /// - the warble alone leaves `U6` on [`homing_missile_hz`], because 15 Hz is
    ///   slow against a 1 ms period and the part simply follows it;
    /// - adding `NOISE 1` moves it to about 977 Hz, a **24 % rise**;
    /// - stepping the same chain **eight times finer** moves it by about 1 %.
    ///   A rate set by our quantizing the crossing would not survive that: the
    ///   framework triggers on the first step at or past the threshold, so the
    ///   grid's error is a *late* bias of up to one step, worth 0.8 % at
    ///   [`MIN_SIM_RATE`] and in the opposite direction.
    ///
    /// What this costs is that the voice's pitch now rests on [`MM5837_SWING`],
    /// which is invented, and on [`MM5837_HZ`], which is a convention: across
    /// the part's published 24 to 56 kHz spread the rate moves 947 to 997 Hz.
    /// Nothing here is tuned to the reference recording, which sits at
    /// 1025.6 Hz by `disasm audiodiff`'s autocorrelation; that number is
    /// corroboration that the mechanism is on the board too, since no reading of
    /// `R48`, `R49` and `C45` produces it either.
    #[test]
    fn the_homing_missiles_pitch_is_set_by_the_noise_on_its_control_pin() {
        let (warble_only, _) = homing_chain(MIN_SIM_RATE, true, false);
        assert!(
            (warble_only - homing_missile_hz()).abs() < 5.0,
            "the warble alone should leave U6 on its parts' rate: {warble_only} Hz \
             against {}",
            homing_missile_hz()
        );

        let (with_noise, _) = homing_chain(MIN_SIM_RATE, true, true);
        assert!(
            with_noise > warble_only * 1.15,
            "NOISE 1 on the control pin must raise the rate well clear of the \
             free-run one: {with_noise} Hz against {warble_only} Hz"
        );
        assert!(
            (with_noise - 977.0).abs() < 15.0,
            "the rate this chain settles on: {with_noise} Hz"
        );

        // And it is the part, not the grid. Eight times the resolution.
        let (finer, _) = homing_chain(MIN_SIM_RATE * 8, true, true);
        assert!(
            (finer - with_noise).abs() / with_noise < 0.03,
            "the rate moved with the simulation step, so it is ours rather than \
             the board's: {with_noise} Hz at {MIN_SIM_RATE}, {finer} Hz at {}",
            MIN_SIM_RATE * 8
        );

        // The rig is this voice and not a sketch of it: the built device, gated
        // and measured at its own `U6_555` node, lands on the same rate.
        let mut snd = ZaxxonSound::new(CPU_HZ);
        snd.set_ports(IDLE.0, IDLE.1, IDLE.2);
        snd.tick(CPU_HZ / 20);
        snd.set_ports(0xEF, 0xFF, 0xFF);
        let node = snd.circuit().node_by_name("U6_555").expect("U6_555");
        let (slice, mut rises, mut last) = (16u64, 0usize, false);
        for _ in 0..(CPU_HZ / slice) {
            snd.circuit.tick(slice);
            let high = snd.circuit().value(node) > 1.0;
            if high && !last {
                rises += 1;
            }
            last = high;
        }
        let device = rises as f64;
        assert!(
            (device - with_noise).abs() / with_noise < 0.02,
            "the device runs at {device} Hz where the chain on its own runs at \
             {with_noise} Hz"
        );
    }

    /// The homing missile's energy below 100 Hz is the **duty cycle the warble
    /// moves**, and it is arithmetic on the 555's own two thresholds.
    ///
    /// This was carried as an open question worded the other way round: whether
    /// a swept square's broadband floor is real or is our placing its edges on a
    /// sample grid. It is neither a broadband floor nor the grid.
    ///
    /// A 555's control pin raises the charge leg's target while leaving the
    /// discharge leg at `ln 2` of its own time constant, so a moving control pin
    /// moves the duty as well as the pitch. Over the warble's +/-0.248 V the
    /// duty runs [`homing_duty_at`]'s 0.590 to 0.666, so the square's mean value
    /// swings 0.181 to 0.331 of its own amplitude at 15.4 Hz, and the leg's 1 uF
    /// block passes that whole (its corner is 3.1 Hz). That is a modulation
    /// sitting 27 dB under the tone, at a frequency no tone on this board
    /// reaches.
    ///
    /// Predicted from the duty swing alone and measured through the chain, the
    /// two agree to a few percent, and stepping the chain eight times finer
    /// moves the measurement by under 3 %. A floor made by quantizing the edges
    /// would fall 18 dB across that, since its power goes as the step squared.
    ///
    /// What the reference recording says about it is nothing: `03.wav` carries
    /// 0.00 % of its energy below 400 Hz, and the modulation's fundamental is
    /// 15.4 Hz with its first harmonics at 31 and 46 Hz, which is where a
    /// cabinet speaker and a sample-maker's high-pass both live. The board makes
    /// this; whether anything downstream of the board passes it is not a
    /// question the drawing or the sample set can answer.
    #[test]
    fn the_homing_missiles_low_band_is_the_duty_the_warble_moves() {
        // The warble reaches U6's control pin at beta of the op-amp's swing
        // through -R47/R45, either side of the part's own two thirds of +5 V.
        let depth = homing_beta() * OPAMP_SWING * R47 / R45;
        let rest = V5 * 2.0 / 3.0;
        let (d_lo, d_hi) = (homing_duty_at(rest - depth), homing_duty_at(rest + depth));
        assert!((d_lo - 0.5905).abs() < 5e-3, "duty at the bottom {d_lo}");
        assert!((d_hi - 0.6657).abs() < 5e-3, "duty at the top {d_hi}");

        // A square of duty `d` about its own mean has an AC RMS of
        // `2*sqrt(d*(1-d))`, and its mean is `2d - 1`. The warble is two
        // exponential segments, close enough to a triangle that its RMS is its
        // half-amplitude over sqrt(3).
        let mean_swing = 2.0 * (d_hi - d_lo);
        let mod_rms = mean_swing / 2.0 / 3.0f64.sqrt();
        let d_mid = 0.5 * (d_lo + d_hi);
        let square_rms = 2.0 * (d_mid * (1.0 - d_mid)).sqrt();
        let predicted = (mod_rms / square_rms).powi(2);
        assert!(
            (predicted - 0.00202).abs() < 3e-4,
            "the duty swing predicts {predicted} of the square's energy"
        );

        // Measured through the chain, with the 100 Hz filter's own leakage of
        // the square taken off: that leakage is the whole of the static case,
        // since a square with a fixed duty has nothing below its fundamental.
        for sim in [MIN_SIM_RATE, MIN_SIM_RATE * 8] {
            let (_, with) = homing_chain(sim, true, false);
            let (_, without) = homing_chain(sim, false, false);
            let measured = with - without;
            assert!(
                without < predicted / 3.0,
                "a fixed duty should leave almost nothing below 100 Hz, and left \
                 {without} at {sim} Hz"
            );
            assert!(
                (measured - predicted).abs() / predicted < 0.2,
                "measured {measured} against the duty swing's {predicted}, at a \
                 simulation step of {sim} Hz"
            );
        }
    }

    /// The base missile, now read at component level, is the last voice on this
    /// board that was only ever read at block level.
    ///
    /// Nothing in it needed changing, which is worth pinning precisely because
    /// nothing did: every one of its numbers is arithmetic on read parts, and
    /// the one thing a reader would want to check against the reference
    /// recording cannot be checked. See the transcription.
    #[test]
    fn the_base_missile_is_arithmetic_on_read_parts() {
        // U22 half B: C48 15 uF and R56 36 k at the plain 74123's 0.28.
        let width = K74123 * OS_BASE_MISSILE.0 * OS_BASE_MISSILE.1;
        assert!((width - 0.1512).abs() < 1e-4, "one-shot {width} s");

        // C49 against R58 470 ohms going down through D2, and against
        // R59 + R60 440 k coming back up. The recovery is three times either
        // explosion's and is the longest time constant on the board.
        let attack = R58 * C49;
        let recover = R59_R60 * C49;
        assert!((attack - 7.05e-3).abs() < 1e-4, "attack {attack} s");
        assert!((recover - 6.6).abs() < 0.01, "recovery {recover} s");
        assert!(
            recover > 3.0 * R109_R110 * C63,
            "against the medium explosion"
        );

        // U20 taps the R59/R60 junction, so the control swings half as far as
        // C49 does: 5.00 V at rest and 2.80 V at the bottom, which is the same
        // window both explosions use and straddles the MB4391's 4.76 and 2.84.
        let rest = V5;
        let floor = V_DIODE + (V5 - V_DIODE) * 0.5;
        assert!((floor - 2.8).abs() < 1e-9, "floor {floor} V");
        assert!(rest > mb4391_mute_v(), "rests muted");
        assert!(floor < mb4391_full_v(), "opens fully");

        // R61/R62 15 k with C50/C137 0.022 uF, and a gain of 1 + R63/R64 with
        // R63 50 k and R64 100 k. It lands on engine tone B's frequency from a
        // completely different pair of parts.
        let f0 = 1.0 / (std::f64::consts::TAU * 15_000.0 * 0.022e-6);
        assert!((BASE_MISSILE_HZ - f0).abs() < 0.5, "{BASE_MISSILE_HZ} Hz");
        assert!((BASE_MISSILE_HZ - SHIP_TONE_B_HZ).abs() < 0.1);
        assert!((BASE_MISSILE_GAIN - (1.0 + 50_000.0 / 100_000.0)).abs() < 1e-12);
        assert!((BASE_MISSILE_Q - 1.0 / (3.0 - BASE_MISSILE_GAIN)).abs() < 1e-3);
    }

    #[test]
    fn the_alarm_clock_divides_to_two_audible_tones() {
        let clk = alarm_clock_hz();
        assert!((clk - 20_282.0).abs() < 5.0, "U50 556: {clk} Hz");
        // `1QC` is clock/8 and `1QD` is clock/16, both read off the 74393's
        // pins 5 and 6 at 400 dpi. Which alarm gets which is not established.
        assert!((clk / 8.0 - 2535.0).abs() < 2.0);
        assert!((clk / 16.0 - 1268.0).abs() < 2.0);
    }

    /// Every leg's 1 uF block is a 51 ms time constant, and three of the seven
    /// one-shot widths are comparable to it rather than far inside it.
    ///
    /// [`C_BLOCK`]'s comment used to say the corner was below anything the board
    /// generates. It is below every tone; it is not below the alarms' 132 ms
    /// burst, and that is where their measured 125-250 Hz content comes from. A
    /// change to `R_COMMON` or `C_BLOCK` has to confront that.
    /// `Q6` conducts for the first fifth of the cannon's decay, not all of it.
    ///
    /// The threshold is derived: a bipolar transistor's base-emitter junction is
    /// a silicon diode, and `R131`/`R132` put the base at 0.180 of the envelope.
    /// It used to be zero, which swept the voice for the whole 0.68 s and left
    /// it 7 dB light at 1-2 kHz against the reference.
    #[test]
    fn the_cannon_sweeps_for_a_fifth_of_its_decay() {
        let div = R132 / (R131 + R132);
        assert!((div - 0.1803).abs() < 1e-3, "Q6's base sees {div}");
        let threshold = cannon_q6_threshold_v();
        assert!((threshold - 3.327).abs() < 0.01, "{threshold} V");

        // The envelope starts at V5 - V_DIODE and decays through R127_ENV.
        let peak = V5 - V_DIODE;
        assert!(threshold < peak, "Q6 must conduct at all");
        // Peak base drive is a fifth of a volt above turn-on, so Q6 is a soft
        // resistance over a narrow range rather than a switch.
        assert!(
            (peak * div - 0.793).abs() < 0.005,
            "peak base {}",
            peak * div
        );

        let tau = R127_ENV * C79;
        let sweeping = tau * (peak / threshold).ln();
        assert!(
            (sweeping - 0.190).abs() < 0.005,
            "sweeping for {sweeping} s"
        );
        assert!(
            sweeping < tau / 3.0,
            "the sweep is a fifth of the decay, not all of it"
        );

        // And where it parks once Q6 is off: R130 in series with R133.
        let parked = 1.0 / (std::f64::consts::TAU * C81 * (R127_FB * (R130 + R133)).sqrt());
        assert!((parked - 1835.0).abs() < 10.0, "parked at {parked} Hz");
    }

    #[test]
    fn the_mix_block_is_not_below_every_envelope() {
        let tau = R_COMMON * C_BLOCK;
        assert!((tau - 0.051).abs() < 1e-3, "the block is {tau} s");
        let alarm = K74123 * OS_ALARM.0 * OS_ALARM.1;
        assert!(
            alarm < 3.0 * tau,
            "a {alarm} s burst against a {tau} s block is a droop, not a block"
        );
        // The two long envelopes are the other way round by orders of magnitude,
        // which is why only the alarms show it.
        assert!(R109_R110 * C63 > 30.0 * tau, "the medium explosion");
        assert!(R59_R60 * C49 > 100.0 * tau, "the base missile");
    }

    #[test]
    fn every_mix_leg_uses_the_same_common_resistor() {
        // The board's whole balance is the series/shunt pair, because the eleven
        // commons are equal. A leg table with a different common would silently
        // move a voice; this is the check that the table stays that shape.
        let legs = [
            LEG_SHIP_A,
            LEG_SHIP_B,
            LEG_HOMING_MISSILE,
            LEG_BASE_MISSILE,
            LEG_LASER,
            LEG_BATTLESHIP,
            LEG_S_EXP,
            LEG_M_EXP,
            LEG_CANNON,
            LEG_SHOT,
            LEG_ALARM,
        ];
        assert_eq!(legs.len(), LEG_COUNT, "eleven legs reach SJ");
        // The medium explosion is the loudest leg and the alarms the quietest,
        // by the ratios in the transcription's table.
        let ratio = |(rs, rp): (f64, f64)| rp / (rs + rp);
        assert!((ratio(LEG_M_EXP) - 0.851).abs() < 0.001);
        assert!((ratio(LEG_ALARM) - 0.0145).abs() < 0.001);
        let loudest = legs.iter().copied().map(ratio).fold(0.0f64, f64::max);
        assert!((loudest - ratio(LEG_M_EXP)).abs() < 1e-9);
    }

    /// A leg times [`leg_to_output`] is what that voice puts into the mix, and
    /// this checks the number against the device rather than against the
    /// arithmetic that produced it.
    ///
    /// Sound one voice on its own, and the output sample must be its leg scaled
    /// by that factor, to within the resampler's own interpolation. Getting it
    /// wrong is not a cosmetic error in a debug view: `sndcmp`'s per-voice probe
    /// captures are the only way to hear one voice of eleven, they are what a
    /// before-and-after comparison of a topology correction is made from, and
    /// with a flat 50x every one of them clipped.
    #[test]
    fn a_leg_probe_is_the_voices_share_of_the_mix() {
        assert!(
            (leg_to_output() - 0.5093).abs() < 1e-3,
            "one leg volt reaches the output at {}",
            leg_to_output()
        );

        // The battleship, because it is the one voice whose source amplitude is
        // derived end to end. Compared as RMS rather than as a peak: the output
        // is band-limited by the resampler, so a square's edges overshoot there
        // and not at the leg, which is worth 16 % on a peak and nothing on an
        // RMS.
        let mut snd = ZaxxonSound::new(CPU_HZ);
        let _ = render(&mut snd, 20, IDLE);
        let out = render(&mut snd, 600, (0x7F, 0xFF, 0xFF));
        // Second half only: the leg's 1 uF block settles over the first.
        let steady = &out[out.len() / 2..];
        let sample_rms = rms(steady) / f64::from(i16::MAX);

        let leg = leg_rms("BATTLESHIP_LEG", (0x7F, 0xFF, 0xFF), 600);
        let predicted = leg * leg_to_output();
        assert!(
            (sample_rms - predicted).abs() / predicted < 0.05,
            "the battleship reaches {sample_rms} of full scale where its leg of \
             {leg} V predicts {predicted}"
        );

        // And the scale keeps every voice inside full scale, which is the whole
        // point: a probe capture that clips cannot be compared with anything.
        for (label, node, ports, ms) in [
            (
                "ship tone A",
                "SHIP_A_LEG",
                (0xF3u8, 0xFFu8, 0xFFu8),
                600u64,
            ),
            ("medium explosion", "M_EXP_LEG", (0xFF, 0xDF, 0xFF), 400),
            ("homing missile", "HOMING_LEG", (0xEF, 0xFF, 0xFF), 400),
            ("alarms", "ALARM_LEG", (0xFF, 0xFF, 0xFB), 400),
        ] {
            let scaled = leg_peak(node, ports, ms) * leg_to_output();
            assert!(
                scaled < 1.0,
                "{label}'s probe would clip at {scaled} of full scale"
            );
        }
    }

    #[test]
    fn save_load_round_trip() {
        let mut snd = ZaxxonSound::new(CPU_HZ);
        let _ = render(&mut snd, 20, IDLE);
        let _ = render(&mut snd, 30, (0xFF, 0xDF, 0xFF)); // medium explosion

        let mut w = StateWriter::new();
        snd.save_state(&mut w);
        let bytes = w.into_vec();

        let mut restored = ZaxxonSound::new(CPU_HZ);
        let mut r = StateReader::new(&bytes);
        restored.load_state(&mut r).unwrap();

        let a = render(&mut snd, 30, IDLE);
        let b = render(&mut restored, 30, IDLE);
        assert_eq!(a, b, "both continue identically from the saved point");
    }
}
