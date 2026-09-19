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
//! Four things are **not**, and each says `INVENTED` in its own doc comment
//! rather than hiding among the read values:
//!
//! - the `MCD-725H` opto-isolator's resistance against LED current, which sets
//!   the player ship's engine pitch;
//! - the `MB4391` VCA's control law beyond its direction;
//! - `Q6`'s collector-emitter resistance against its base drive, which sweeps
//!   the cannon;
//! - the battleship's and the shot's oscillator pitches, whose chains were read
//!   at block level only.
//!
//! # There is no reference to compare against
//!
//! The reference emulator plays recorded WAV samples for this board, so a
//! comparison would measure whoever made the recordings rather than the
//! hardware. Same situation as Congo Bongo's percussion, and the reasoning is in
//! `docs/schematics/congo-percussion.md`. The catalog row in
//! `tools/sound-compare/targets.toml` says `implemented-unvalidated` for that
//! reason and cannot honestly say more.

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
/// is a guess rather than a divider, and everything downstream of it is scaled
/// by read resistors, so it sets the absolute level and nothing else.
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

/// `PC1`'s LED forward drop. Below this the LED is dark, which is what the
/// lowest of the four levels produces.
const PC1_LED_VF: f64 = 1.2;

/// **INVENTED.** `PC1`'s photoresistance at the brightest of the four levels,
/// its dark value, and the exponent it falls with between them.
///
/// The `MCD-725H`'s transfer curve is not on the drawing and no datasheet was
/// found for it, so this is the one part of the engine voice that is a model
/// choice rather than a reading. A CdS cell's resistance falls close to a power
/// law in illumination; these three numbers make the engine sweep 232, 397, 601
/// and 750 Hz across the ladder's four levels.
///
/// What the drawing *does* fix, and what a change here must preserve: with the
/// LED dark the input resistance is `R20` alone and the center frequency is
/// `1 / (2*pi*C26*sqrt(R20*R21))` = 232 Hz. That is the lowest of the four and
/// it is not adjustable here.
const PC1_R_BRIGHT: f64 = 1_000.0;
const PC1_EXPONENT: f64 = 1.05;
/// Dark resistance, which is also the ceiling the power law is clamped to.
const PC1_R_DARK: f64 = 5_000_000.0;

/// The two Wien resonators the engine tone is rung at (sheet 12, p135 zone D4).
///
/// `1 / (2*pi*R*C)` with `R24`/`R25` = `R38`/`R39` = 100 k, and an amplifier
/// gain of 2 from the equal 2.2 k pairs, so `Q = 1/(3 - K)` = 1.
const SHIP_TONE_A_HZ: f64 = 723.4; // C30, C31 2200 pF
const SHIP_TONE_B_HZ: f64 = 482.3; // C39, C40 3300 pF
const SHIP_TONE_Q: f64 = 1.0;

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

// `R127` really does appear twice on sheet 11, once as the 100 k envelope shunt
// and once as the 47 k band-pass feedback, both legible at 400 dpi. One of them
// is presumably `R129`, which appears nowhere. The two names above distinguish
// them by function because the drawing does not.

/// **INVENTED.** `Q6`'s collector-emitter resistance at full envelope and at
/// rest.
///
/// The drawing gives `R130` 100 ohm in series with `Q6` from the band-pass's
/// tuning node to ground, and `R131` 15 k / `R132` 3.3 k into its base, but
/// nothing about the transistor's transfer. These two endpoints make the voice
/// sweep from about 7.4 kHz at the onset down to 734 Hz as the envelope decays,
/// which is the descending crack a cannon is; the shape between them is a power
/// law of the same kind used for the LDR.
const CANNON_R_Q6_ON: f64 = 10.0;
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
const R173: f64 = 330_000.0; // U12 feedback: a gain of 220
const C99: f64 = 0.01e-6; // across it: a 48 Hz corner, so the square integrates

// ---------------------------------------------------------------------------
// The battleship and the shot: read at block level only
// ---------------------------------------------------------------------------

/// The battleship's modulation rate, from the `U9` integrator/Schmitt pair:
/// `R88 / (4 * R86 * R82 * C)` with `R82` 30 k, `R86` 51 k, `R88` 100 k and
/// `C56` in series with `C57` (3.3 uF each, so 1.65 uF).
const BATTLESHIP_MOD_HZ: f64 = 9.9;

/// **INVENTED.** The battleship's audible pitch.
///
/// The `U9`/`U10`/`Q4`/`Q5` chain on sheet 11 was read as a part list and not
/// solved, so the 9.9 Hz above is the only figure here that comes off the
/// drawing. A low rumble is what the voice is for; this number is not evidence.
const BATTLESHIP_HZ: f64 = 62.0;
const BATTLESHIP_Q: f64 = 1.2;

/// The shot's pitch, from `R156`/`R157` 33 k with `C92` 1000 pF around `U19`:
/// `1 / (2*pi*R*C)`.
///
/// Derived on the assumption that `U19`'s section has the same second-order
/// shape as the board's other filters, which was **not** traced. Treat it as
/// better than a guess and worse than a reading.
const SHOT_HZ: f64 = 4_823.0;
const SHOT_Q: f64 = 3.0;

/// **INVENTED.** How fast the shot's tone burst decays after its 11 ms one-shot.
const SHOT_DECAY_S: f64 = 0.035;

// ---------------------------------------------------------------------------
// The three sheet-12 voices
// ---------------------------------------------------------------------------

const R44: f64 = 6_800.0; // homing missile envelope
const C43: f64 = 6.8e-6; // -> 46 ms

const R48: f64 = 47_000.0; // U6 555, the homing missile's swept tone
const R49: f64 = 68_000.0;
const C45: f64 = 0.01e-6;
/// `1.44 / ((R48 + 2*R49) * C45)`: the free-running pitch `C46` modulates.
fn homing_missile_hz() -> f64 {
    1.44 / ((R48 + 2.0 * R49) * C45)
}
/// **INVENTED.** How far the envelope on `U6`'s control-voltage pin pulls that
/// pitch. The `U4` summing stage ahead of it is read (`R45` 68 k, `R46` 200 k,
/// `R47` 10 k, `C46`), but a 555's control pin sweeps by an amount that depends
/// on the source impedance, which was not worked out.
const HOMING_MISSILE_SWEEP: f64 = 0.55;

/// `U6` runs on +5 V (pins 4 and 8), so its square output swings to about
/// `Vcc - 1.2`, and `R51`/`R52` divide that before `C47` and the 4016B.
const R51: f64 = 12_000.0;
const R52: f64 = 3_300.0;

const R58: f64 = 470.0; // base missile envelope discharge
const C49: f64 = 15e-6;
const R59_R60: f64 = 440_000.0; // its recovery -> 6.6 s
/// The third Sallen-Key noise band, on sheet 12: `R61`/`R62` 15 k with
/// `C50`/`C137` 0.022 uF, gain `1 + R63/R64` = 1.5, so `Q = 1/(3 - K)` = 0.67.
const BASE_MISSILE_HZ: f64 = 482.3;
const BASE_MISSILE_Q: f64 = 0.667;
const BASE_MISSILE_GAIN: f64 = 1.5;

const R65: f64 = 5_100.0; // U7 555, the laser's repetition rate
const R66: f64 = 22_000.0;
const C53: f64 = 10e-6;
/// `U7`'s repetition rate. `D3` shunts `R66` on the charge, so it is
/// `1.44 / ((R65 + 2*R66) * C53)` in period terms with a 19 % duty cycle: about
/// 5.3 Hz, a repetition rate rather than a tone.
fn laser_repeat_hz() -> f64 {
    1.44 / ((R65 + 2.0 * R66) * C53)
}
/// **INVENTED.** The laser's audible pitch and decay. The `U8`/`Q3`/`D4` chain
/// `U7` drives was read as a part list and not solved.
const LASER_HZ: f64 = 1_450.0;
const LASER_DECAY_S: f64 = 0.055;

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

/// The 1 uF block against the 51 kOhm common: a 3.1 Hz corner, far below
/// anything the board generates, so it is here for the DC offsets the VCAs and
/// switches leave behind rather than for tone.
const C_BLOCK: f64 = 1e-6;

/// Final scaling into the resampler.
///
/// **A headroom choice, not a reading.** The `LA4460`'s closed-loop gain is set
/// by `C12`/`R4` on a pin the drawing does not dimension, and the volume
/// potentiometer `VR1` is an operator control with no defined position, so there
/// is no voltage on this board that corresponds to full scale. This puts a
/// single loud voice at roughly a third of full scale and leaves room for the
/// several that overlap in play.
const OUTPUT_GAIN: f64 = 3.2;

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
/// Returns the **control** node, which is the midpoint of the two-resistor
/// divider between the rail and the capacitor, not the capacitor itself.
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
            full_v: (ship_levels()[0] - PC1_LED_VF) * 1000.0 / R17,
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

    // The two Wien resonators, each gated by one 74LS139 output.
    let mut ship_legs = Vec::new();
    for (name, gate, hz, leg) in [
        ("SHIP_A", ship_tone_a, SHIP_TONE_A_HZ, LEG_SHIP_A),
        ("SHIP_B", ship_tone_b, SHIP_TONE_B_HZ, LEG_SHIP_B),
    ] {
        let tone = b.second_order(
            &format!("{name}_WIEN"),
            ship_bp,
            FilterMode::BandPass,
            hz,
            SHIP_TONE_Q,
        );
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
            threshold_v: 0.0,
            full_v: V5 - V_DIODE,
            r_dark: CANNON_R_Q6_OFF,
            r_min: CANNON_R_Q6_ON,
            exponent: PC1_EXPONENT,
        }),
    );
    let cannon_tune = b.gain("CANNON_TUNE", q6, 1.0);
    let cannon_r130 = b.constant("R130", R130);
    let cannon_leg_r = b.add("CANNON_RTUNE", &[cannon_tune, cannon_r130]);
    let cannon_bp = b.custom(
        "U12_CANNON_BP",
        vec![noise2, cannon_leg_r],
        Box::new(TunedBandPass::new(R128, R127_FB, C81)),
    );
    // The band-pass is always live, so something downstream has to stop it
    // hissing between shots: `MB4391 U13` ch B, which `C84` feeds. Where its
    // control pin comes from was NOT traced, and the same envelope inverted is
    // the only source on the sheet that leaves the board silent at rest.
    let cannon_ctrl_neg = b.gain("CANNON_VCA_NEG", cannon_env, -1.0);
    let cannon_ctrl_rest = b.constant("CANNON_VCA_REST", mb4391_mute_v());
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
    let shot_env = b.rc_envelope("SHOT_ENV", shot_pulse, 1e-4, SHOT_DECAY_S);
    let shot_tone = b.second_order("SHOT_TONE", noise2, FilterMode::BandPass, SHOT_HZ, SHOT_Q);
    let shot_voice = b.multiply("SHOT_OUT", shot_tone, shot_env);
    let shot_leg = mix_leg(&mut b, "SHOT_LEG", shot_voice, LEG_SHOT);

    // --- The battleship ------------------------------------------------------
    // The 4016B at U17 is a switch, not a VCA: it either passes the oscillator
    // or removes its leg from the network. Modeled as a gate on the source,
    // which is not the same thing -- see the note on `resistor_mixer_switched`
    // in the framework -- but the board's legs are all 51k into a 10k load, so
    // opening one changes the others by under half a decibel.
    let bs_carrier = b.triangle("BATTLESHIP_OSC", BATTLESHIP_HZ);
    let bs_mod = b.triangle("BATTLESHIP_MOD", BATTLESHIP_MOD_HZ);
    let bs_depth = b.gain("BATTLESHIP_DEPTH", bs_mod, 0.4);
    let bs_unity = b.constant("BATTLESHIP_UNITY", 0.6);
    let bs_env = b.add("BATTLESHIP_ENV", &[bs_depth, bs_unity]);
    let bs_shaped = b.multiply("BATTLESHIP_AM", bs_carrier, bs_env);
    let bs_band = b.second_order(
        "BATTLESHIP_BAND",
        bs_shaped,
        FilterMode::LowPass,
        BATTLESHIP_HZ * 3.0,
        BATTLESHIP_Q,
    );
    // U10's output reaches the 4016B through C59 with no divider, so this voice
    // arrives at its leg at the full op-amp swing. Its leg is correspondingly
    // one of the smallest on the board, at 0.0909.
    let bs_level = b.gain("BATTLESHIP_LEVEL", bs_band, OPAMP_SWING);
    let bs_gated = b.multiply("BATTLESHIP_SW", bs_level, battleship);
    let battleship_leg = mix_leg(&mut b, "BATTLESHIP_LEG", bs_gated, LEG_BATTLESHIP);

    // --- The homing missile: a 555 swept by an RC envelope -------------------
    let hm_env = b.rc_envelope("HOMING_ENV", homing_missile, R44 * C43, R44 * C43);
    let hm_sweep = b.gain(
        "HOMING_SWEEP",
        hm_env,
        homing_missile_hz() * HOMING_MISSILE_SWEEP,
    );
    let hm_base = b.constant("HOMING_BASE", homing_missile_hz());
    let hm_freq = b.add("HOMING_FREQ", &[hm_base, hm_sweep]);
    let hm_tone = b.variable_square("HOMING_TONE", hm_freq);
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
    let bm_control = inverted_envelope(&mut b, "BASE_MISSILE_ENV", bm_pulse, V6, R59_R60, R58, C49);
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
    let laser_repeat = b.fixed_square("LASER_REPEAT", laser_repeat_hz());
    let laser_env = b.rc_envelope("LASER_ENV", laser_repeat, 1e-4, LASER_DECAY_S);
    let laser_carrier = b.triangle("LASER_TONE", LASER_HZ);
    let laser_voice = b.multiply("LASER_AM", laser_carrier, laser_env);
    // U8's output divided by R75/R76 before C55 and the 4016B.
    let laser_level = b.gain("LASER_LEVEL", laser_voice, OPAMP_SWING * R76 / (R75 + R76));
    let laser_gated = b.multiply("LASER_SW", laser_level, laser);
    let laser_leg = mix_leg(&mut b, "LASER_LEG", laser_gated, LEG_LASER);

    // --- The alarms: one 556, one 74393, two 7426 sections -------------------
    let alarm_clk = b.constant("U50_556", alarm_clock_hz());
    let divider = b.ripple_counter("U49_74393", alarm_clk, 4);
    let qc = b.bit_decode("U49_1QC", divider, 2); // clock / 8
    let qd = b.bit_decode("U49_1QD", divider, 3); // clock / 16
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
    // Both 7426 sections are open-collector onto one node pulled up by R171, so
    // the node is the AND of the two NANDs. Which alarm is paired with which
    // divider tap is not established; see the transcription.
    let a2_gate = b.logic_gate("U67_A", LogicOp::Nand, a2_pulse, qc);
    let a3_gate = b.logic_gate("U67_B", LogicOp::Nand, a3_pulse, qd);
    let alarm_node = b.logic_gate("U67_WIRED_AND", LogicOp::And, a2_gate, a3_gate);
    // Expressed as the fall from `R171`'s pull-up rather than as an absolute
    // voltage, for the reason `inverted_envelope` gives: the node rests high, so
    // a model referenced to zero would push a step through `C24` at power-on
    // that the board's mute circuit exists to cover.
    let alarm_swing = b.logic_levels("ALARM_SWING", alarm_node, -(V12 - V_SAT), 0.0);
    let alarm_amp = b.gain("U12_ALARM_AMP", alarm_swing, -R173 / R172);
    let alarm_int = b.low_pass_hz(
        "U12_ALARM_INT",
        alarm_amp,
        1.0 / (std::f64::consts::TAU * R173 * C99),
    );
    // U12 runs on the single +12 V supply against a +6 V reference, so it has
    // six volts of headroom above its resting rail and no more.
    let alarm_clipped = b.clamp("U12_ALARM_CLIP", alarm_int, 0.0, V6);
    let alarm_leg = mix_leg(&mut b, "ALARM_LEG", alarm_clipped, LEG_ALARM);

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
    #[test]
    fn voice_levels_follow_the_leg_table() {
        /// A voice, its leg node, the latches that drive it, and how long to
        /// hold them for.
        type Case = (&'static str, &'static str, (u8, u8, u8), u64);
        let cases: [Case; 11] = [
            ("ship tone A", "SHIP_A_LEG", (0xF3, 0xFF, 0xFF), 600),
            ("ship tone B", "SHIP_B_LEG", (0xF7, 0xFF, 0xFF), 600),
            ("homing missile", "HOMING_LEG", (0xEF, 0xFF, 0xFF), 400),
            ("base missile", "BASE_MISSILE_LEG", (0xDF, 0xFF, 0xFF), 400),
            ("laser", "LASER_LEG", (0xBF, 0xFF, 0xFF), 700),
            ("battleship", "BATTLESHIP_LEG", (0x7F, 0xFF, 0xFF), 600),
            ("small explosion", "S_EXP_LEG", (0xFF, 0xEF, 0xFF), 400),
            ("medium explosion", "M_EXP_LEG", (0xFF, 0xDF, 0xFF), 400),
            ("cannon", "CANNON_LEG", (0xFF, 0x7F, 0xFF), 400),
            ("shot", "SHOT_LEG", (0xFF, 0xFF, 0xFE), 200),
            ("alarms", "ALARM_LEG", (0xFF, 0xFF, 0xFB), 400),
        ];

        let peaks: Vec<(&str, f64)> = cases
            .iter()
            .map(|(label, node, ports, ms)| (*label, leg_peak(node, *ports, *ms)))
            .collect();
        let summary: Vec<String> = peaks
            .iter()
            .map(|(l, p)| format!("{l} {:.0} mV", p * 1000.0))
            .collect();
        let summary = summary.join(", ");

        let loudest = peaks.iter().map(|(_, p)| *p).fold(0.0f64, f64::max);
        let quietest = peaks.iter().map(|(_, p)| *p).fold(f64::MAX, f64::min);
        assert!(quietest > 0.0, "every voice must reach SJ. {summary}");
        // The leg table itself spans 59:1 from the medium explosion to the
        // alarms, and the alarms sit behind a gain of 220, so some spread is the
        // board. A hundredfold is not.
        assert!(
            loudest / quietest < 100.0,
            "the mix spans {:.0}:1, which is wider than the leg table can \
             account for; a voice's source amplitude is doing the work the \
             series/shunt pairs should be doing. {summary}",
            loudest / quietest
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

    #[test]
    fn the_alarm_clock_divides_to_two_audible_tones() {
        let clk = alarm_clock_hz();
        assert!((clk - 20_282.0).abs() < 5.0, "U50 556: {clk} Hz");
        // 1QC is clock/8 and 1QD is clock/16.
        assert!((clk / 8.0 - 2535.0).abs() < 2.0);
        assert!((clk / 16.0 - 1268.0).abs() < 2.0);
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
        assert_eq!(legs.len(), 11, "eleven legs reach SJ");
        // The medium explosion is the loudest leg and the alarms the quietest,
        // by the ratios in the transcription's table.
        let ratio = |(rs, rp): (f64, f64)| rp / (rs + rp);
        assert!((ratio(LEG_M_EXP) - 0.851).abs() < 0.001);
        assert!((ratio(LEG_ALARM) - 0.0145).abs() < 0.001);
        let loudest = legs.iter().copied().map(ratio).fold(0.0f64, f64::max);
        assert!((loudest - ratio(LEG_M_EXP)).abs() < 1e-9);
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
