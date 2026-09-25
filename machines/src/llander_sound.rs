//! Lunar Lander (1979) discrete sound, built on the [`DiscreteCircuit`]
//! framework.
//!
//! A pair of 74LS164 shift registers form a 16-bit XNOR noise source clocked at
//! 12 kHz. Its output feeds three analog switches whose resistors set the rocket
//! thrust's volume, a resonant band-pass at 89.5 Hz makes the rumble, a fourth
//! switch adds the crash explosion from the same node, and two fixed squares at
//! 3 kHz and 6 kHz are the low-fuel and slam alerts. All four sum into an LM324
//! mixer whose leg resistors set their balance. The board talks to this with
//! hardware intent (`write_sound_register`, `pulse_noise_reset`).
//!
//! Transcribed from the drawing in
//! [`docs/schematics/llander-audio-output.md`](../../docs/schematics/llander-audio-output.md).
//! Every component value named below is from that sheet.
//!
//! # The throttle is a filter as well as a volume
//!
//! The three switched resistors that set the thrust's volume also set the
//! corner of the low-pass `C15` makes on their common node, so quieter thrust
//! is darker thrust: 73 Hz at full throttle, 14 Hz at throttle 1. The volume
//! law is compressed rather than linear, putting throttle 1 about 2.6 dB above
//! where a linear control would. [`Throttle`] takes each setting's time
//! constant and gain from `llander_sound_derived.rs`, which `netlist derive`
//! solves from the board's transcription.
//!
//! No comparison against the reference can check this. Its netlist has a fixed
//! corner and a linear volume, as this device did, so the two agreed to 0.15
//! percentage points on every band at every setting while both were wrong in
//! the same way. Only the drawing says so.
//!
//! # Built in volts from the parts, and quieter than the reference
//!
//! The analog block after the throttle is the drawing's, in volts from the
//! +5 V reference: the band-pass from `R22`, `R26`, `R27`, `C20` and `C21`,
//! the two noise legs into the summing amp through `R28` and through `R21`
//! with `C91`, and `C27` across `R31`. Nothing in it is fitted. Two levels are
//! taken on trust, the noise's 3.8 V and the tone gates' 4 V, because the
//! drawing gives none.
//!
//! Against the reference this makes the thrust about 4.6 dB quieter relative
//! to the tones, and the explosion about 11 dB UNDER the thrust where the
//! reference has it 15 dB over. Both reference levels are its author's: its
//! gain table gives the explosion the thrust leg's 6.8k at a flat gain, and
//! calls its thrust level a tweak. A crash as the game plays it is therefore
//! mostly the thrust's roar at full throttle fading as the game steps the
//! throttle down, with the explosion a brighter layer beneath it.

use phosphor_core::core::debug::{DebugRegister, Debuggable};
use phosphor_core::core::save_state::{SaveError, StateReader, StateWriter};
use phosphor_core::device::{
    CustomComponent, DataInputId, DiscreteCircuit, DiscreteCircuitBuilder, LogicInputId, NodeId,
    OutputGain, PulseInputId,
};
use phosphor_macros::Saveable;

use crate::atari_dvg::TIMING;

// ---------------------------------------------------------------------------
// Shared 12 kHz LFSR noise (custom escape-hatch component)
// ---------------------------------------------------------------------------

/// The 16-bit shift register at M6 and M7, clocked at 12 kHz, with XNOR
/// feedback from bits 6 and 14 and its output taken from bit 14.
///
/// Those two taps are M6's QG and M7's QG on the drawing, and the XNOR is built
/// from an LS32 and two LS00 sections rather than an XNOR gate; the truth table
/// is in the transcription. A shift register with the wrong feedback does not
/// fail, it runs a different and usually far shorter polynomial, which is why
/// the taps are worth stating.
///
/// It feeds both the thrust and explosion paths, so it lives on the framework's
/// `Custom` escape hatch (the built-in `lfsr_noise` node can't be reset).
/// Input: `[noise_reset 0/1]`.
struct LanderNoise {
    lfsr: u16,
    clock_acc: f64,
}

impl LanderNoise {
    // For an XNOR register the ALL-ONES state is the lock state, so 0 is the
    // natural running seed. `NOISERESET` is the active-low clear on both LS164s.
    const SEED: u16 = 0;
}

impl CustomComponent for LanderNoise {
    fn reset(&mut self) {
        self.lfsr = Self::SEED;
        self.clock_acc = 0.0;
    }

    fn step(&mut self, inputs: &[f64], dt: f64) -> f64 {
        if inputs[0] > 0.5 {
            self.lfsr = 0;
        }
        self.clock_acc += 12_000.0 * dt;
        while self.clock_acc >= 1.0 {
            self.clock_acc -= 1.0;
            let fb = !(((self.lfsr >> 6) ^ (self.lfsr >> 14)) & 1) & 1;
            self.lfsr = (self.lfsr << 1) | fb;
        }
        if (self.lfsr >> 14) & 1 != 0 {
            1.0
        } else {
            -1.0
        }
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
// The throttle: three switched legs and C15 on one node (custom component)
// ---------------------------------------------------------------------------

// Solved from the transcription by `netlist derive`; see the file's header for
// how to regenerate it.
#[path = "llander_sound_derived.rs"]
mod derived;

/// Each throttle setting's `C15` time constant, indexed by the 3-bit value.
const THROTTLE_TAU: [f64; 8] = [
    derived::THROTTLE_0_C15_TAU,
    derived::THROTTLE_1_C15_TAU,
    derived::THROTTLE_2_C15_TAU,
    derived::THROTTLE_3_C15_TAU,
    derived::THROTTLE_4_C15_TAU,
    derived::THROTTLE_5_C15_TAU,
    derived::THROTTLE_6_C15_TAU,
    derived::THROTTLE_7_C15_TAU,
];

/// Each throttle setting's DC gain from the noise to the common node.
const THROTTLE_GAIN: [f64; 8] = [
    derived::THROTTLE_0_GAIN,
    derived::THROTTLE_1_GAIN,
    derived::THROTTLE_2_GAIN,
    derived::THROTTLE_3_GAIN,
    derived::THROTTLE_4_GAIN,
    derived::THROTTLE_5_GAIN,
    derived::THROTTLE_6_GAIN,
    derived::THROTTLE_7_GAIN,
];

/// The common node the throttle legs drive, relative to +5 V, as a one-pole
/// section whose time constant and gain both come from the setting.
///
/// Neither is linear in the setting, because the legs are resistors in
/// parallel: throttle 1 is `R18` alone at 11.5 ms, throttle 7 is all three at
/// 2.2 ms. Throttle 0 opens every leg, and the node then drains toward +5 V
/// through the band-pass input at 48 ms rather than stopping, which is why the
/// state is carried across a change of setting instead of being reset.
///
/// The output is the node's own voltage per volt of noise: 0.955 at full
/// throttle, 0.762 at throttle 1.
///
/// A custom component because the builder's one-pole sections have a fixed
/// time constant, and this one switches with a register write. The update is
/// the same backward-Euler step `RcLowPass` takes.
///
/// Inputs: `[noise, throttle setting 0..=7]`.
struct Throttle {
    y: f64,
}

impl CustomComponent for Throttle {
    fn reset(&mut self) {
        self.y = 0.0;
    }

    fn step(&mut self, inputs: &[f64], dt: f64) -> f64 {
        let setting = (inputs[1].round() as usize).min(7);
        let target = inputs[0] * THROTTLE_GAIN[setting];
        let alpha = dt / (THROTTLE_TAU[setting] + dt);
        self.y += alpha * (target - self.y);
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
// Typed input handles + node ids for debug
// ---------------------------------------------------------------------------

struct LunarLanderInputs {
    thrust_data: DataInputId,
    tone3k_en: LogicInputId,
    tone6k_en: LogicInputId,
    explod_en: LogicInputId,
    noise_reset: PulseInputId,
    mix: NodeId,
}

// ---------------------------------------------------------------------------
// Circuit construction
// ---------------------------------------------------------------------------

// Every analog node below is in VOLTS, measured from the +5 V rail that every
// LM324 section's non-inverting input sits on. That rail is the block's signal
// reference, so an AC signal about it is what the circuit carries, and DC is
// blocked before the mix on both noise legs (C20 and C91).

// Part values, as drawn and as transcribed in
// docs/schematics/netlists/llander-audio.toml.
const R21: f64 = 1_500.0;
const R22: f64 = 47_000.0;
const R26: f64 = 1_200.0;
const R27: f64 = 270_000.0;
const R28: f64 = 6_800.0;
const R29: f64 = 390_000.0;
const R31: f64 = 10_000.0;
const R34: f64 = 10_000.0;
const C20: f64 = 0.1e-6;
const C21: f64 = 0.1e-6;
const C27: f64 = 0.1e-6;
const C91: f64 = 0.047e-6;

/// The noise register's output swing, 0 V to this. NOT READ FROM THE BOARD: the
/// drawing gives no level, and 3.8 V is the reference netlist's figure, taken
/// on trust as `docs/schematics/llander-audio-output.md` records. It sets the
/// noise voices against the tones directly.
const NOISE_SWING_V: f64 = 3.8;

/// The tone gates' output swing. Taken on trust from the same source as
/// [`NOISE_SWING_V`], for the same reason.
const GATE_SWING_V: f64 = 4.0;

/// How far an LM324 output can swing symmetrically about the +5 V reference:
/// down to ground is 5 V, and up to the +22 V rail less its 1.5 V headroom is
/// 15.5 V, so the negative side bounds a symmetric signal.
const OPAMP_SWING_V: f64 = 5.0;

/// The ground and the positive limit, relative to the +5 V reference.
const OPAMP_LO_V: f64 = -5.0;
const OPAMP_HI_V: f64 = 15.5;

fn build_circuit() -> (DiscreteCircuit, LunarLanderInputs) {
    let mut b = DiscreteCircuitBuilder::new(
        TIMING.cpu_clock_hz,
        phosphor_core::audio::host_sample_rate() as u64,
    );

    // --- Board-facing inputs ---
    let thrust_data = b.data_input("THRUST_DATA", 1.0); // the 3-bit setting, 0..=7
    let tone3k_en = b.logic_input("TONE3K_EN");
    let tone6k_en = b.logic_input("TONE6K_EN");
    let explod_en = b.logic_input("EXPLOD_EN");
    let noise_reset = b.pulse_input("NOISE_RESET");

    // --- Shared noise -> the throttle's common node ---
    let noise = b.custom(
        "NOISE",
        vec![noise_reset.into()],
        Box::new(LanderNoise {
            lfsr: LanderNoise::SEED,
            clock_acc: 0.0,
        }),
    );
    // The register swings 0 V to NOISE_SWING_V; `LanderNoise` is +/-1, so half
    // the swing makes it the AC part in volts. Its DC never reaches the mix.
    let noise_v = b.gain("NOISE_V", noise, NOISE_SWING_V / 2.0);
    let common = b.custom(
        "THROTTLE",
        vec![noise_v, thrust_data.into()],
        Box::new(Throttle { y: 0.0 }),
    );

    // --- Thrust: the drawn multiple-feedback band-pass on R7 section 2. The
    // common node enters through R22 with R26 to the reference, so the builder
    // takes the divider and the Thevenin resistance from the two parts; with
    // R27, C20 and C21 that is 89.5 Hz, Q 7.6 and a center gain of 2.87 from the
    // common node. ---
    let thrust_bp = b.op_amp_band_pass(
        "THRUST_BP",
        common,
        &[R22, R26],
        R27,
        C20,
        C21,
        0.0,
        OPAMP_LO_V,
        OPAMP_HI_V + 1.5, // the builder takes the rail and subtracts the headroom
    );

    // --- The summing amp on R7 section 3. Each leg is a current into its
    // virtual ground, and the feedback is R31 with C27 across it, so each leg's
    // voltage at AUDIO1 is its current times R31 through C27's 159 Hz pole. The
    // pole is linear, so it is applied per leg; that gives each voice a node of
    // its own at the level the mixer puts it at, and sums to the same thing.
    // The inversion is left out: it is common to both legs and inaudible. ---
    let thrust_i = b.gain("THRUST_I", thrust_bp, R31 / R28);
    let thrust_leg = b.low_pass_tau("THRUST_LEG", thrust_i, R31 * C27);

    // The explosion: P5 section A takes the common node itself, so the throttle
    // is its volume, and R21 in series with C91 makes the current a high-pass
    // at 2258 Hz. With C27's pole the leg is flat at C91/C27 = 0.47 from 159 Hz
    // to 2258 Hz. The switch gates the current: an open switch stops it, and
    // C91 holds its charge.
    let explod_hp = b.high_pass_tau("EXPLOD_HP", common, R21 * C91);
    let explod_i = b.gain("EXPLOD_I", explod_hp, R31 / R21);
    let explod_gated = b.multiply("EXPLOD_GATE", explod_i, explod_en);
    let explod_leg = b.low_pass_tau("EXPLOD_LEG", explod_gated, R31 * C27);

    let audio1_sum = b.add("AUDIO1_SUM", &[thrust_leg, explod_leg]);
    let audio1 = b.clamp("AUDIO1", audio1_sum, OPAMP_LO_V, OPAMP_HI_V);

    // --- Alert tones, into R7 section 4 through 390k each. `fixed_square` is
    // +/-1, so half the gate swing gives a square of GATE_SWING_V peak to peak,
    // and R34/R29 is the inverter's gain for it. ---
    let tone_gain = GATE_SWING_V / 2.0 * R34 / R29;
    let tone3k = b.fixed_square("TONE3K", 3_000.0);
    let tone3k_g = b.multiply("TONE3K_G", tone3k, tone3k_en);
    let tone3k_out = b.gain("TONE3K_OUT", tone3k_g, tone_gain);

    let tone6k = b.fixed_square("TONE6K", 6_000.0);
    let tone6k_g = b.multiply("TONE6K_G", tone6k, tone6k_en);
    let tone6k_out = b.gain("TONE6K_OUT", tone6k_g, tone_gain);

    // --- The mix, AUDIO1 against AUDIO2. AUDIO2 is AUDIO1 inverted at unity
    // plus the tones, so across the pair the noise voices count twice and the
    // tones once; that factor of two is the drawing's. ---
    let noise_pair = b.gain("NOISE_PAIR", audio1, 2.0);
    let mix = b.add("MIX", &[tone3k_out, tone6k_out, noise_pair]);
    b.output(
        mix,
        OutputGain::linear(1.0 / LunarLanderDiscreteSound::MIX_FULL_SCALE),
    );

    let circuit = b.build();
    (
        circuit,
        LunarLanderInputs {
            thrust_data,
            tone3k_en,
            tone6k_en,
            explod_en,
            noise_reset,
            mix,
        },
    )
}

// ---------------------------------------------------------------------------
// LunarLanderDiscreteSound — board-facing wrapper
// ---------------------------------------------------------------------------

/// Concrete Lunar Lander sound device. Wraps a [`DiscreteCircuit`] and exposes
/// hardware-intent methods for the board's bus writes.
#[derive(Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct LunarLanderDiscreteSound {
    #[save(id = 2)]
    circuit: DiscreteCircuit,
    /// Input handles, fixed when the circuit is built.
    #[save_skip]
    ids: LunarLanderInputs,
    /// Last value written to the 0x3C00 sound register (for debug/save).
    #[save(id = 1)]
    sound_reg: u8,
}

impl LunarLanderDiscreteSound {
    /// The `MIX` value, in volts across the AUDIO1/AUDIO2 pair, that renders
    /// as a full-scale output sample: the largest symmetric swing the pair can
    /// make, which is AUDIO1 at its LM324 limit of 5 V about the reference,
    /// doubled across the pair.
    ///
    /// Derived rather than chosen, like the value it replaced. That was the
    /// sum of the reference netlist's four leg levels, and before it a 14347
    /// tuned against a capture that put the whole board 25 dB down. Both were
    /// in units no part on the drawing has; this is in volts.
    ///
    /// Also exposed so a per-stage probe can be read at the same scale the
    /// mixer puts it at. A probe divided by anything else is measuring its own
    /// normalization rather than the voice's share of the mix.
    pub const MIX_FULL_SCALE: f64 = 2.0 * OPAMP_SWING_V;

    pub fn new() -> Self {
        let (circuit, ids) = build_circuit();
        Self {
            circuit,
            ids,
            sound_reg: 0,
        }
    }

    /// 0x3C00 sound register: bits 0-2 thrust volume, bit 3 explosion enable,
    /// bit 4 3 kHz tone enable, bit 5 6 kHz tone enable.
    pub fn write_sound_register(&mut self, data: u8) {
        self.sound_reg = data;
        self.circuit
            .set_data(self.ids.thrust_data, (data & 0x07) as f64);
        self.circuit.set_logic(self.ids.explod_en, data & 0x08 != 0);
        self.circuit.set_logic(self.ids.tone3k_en, data & 0x10 != 0);
        self.circuit.set_logic(self.ids.tone6k_en, data & 0x20 != 0);
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

    /// The built circuit, so the `sndcmp` adapter can render one named node
    /// instead of the mix.
    pub fn circuit(&self) -> &DiscreteCircuit {
        &self.circuit
    }

    pub fn reset(&mut self) {
        self.circuit.reset();
        self.sound_reg = 0;
    }
}

impl Default for LunarLanderDiscreteSound {
    fn default() -> Self {
        Self::new()
    }
}

impl Debuggable for LunarLanderDiscreteSound {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "THRUST_DATA",
                value: (self.sound_reg & 0x07) as u64,
                width: 8,
            },
            DebugRegister {
                name: "EXPLODE",
                value: (self.sound_reg & 0x08 != 0) as u64,
                width: 8,
            },
            DebugRegister {
                name: "TONE_3K",
                value: (self.sound_reg & 0x10 != 0) as u64,
                width: 8,
            },
            DebugRegister {
                name: "TONE_6K",
                value: (self.sound_reg & 0x20 != 0) as u64,
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
    use phosphor_core::core::save_state::Saveable as _;
    fn run_frame(s: &mut LunarLanderDiscreteSound) {
        s.tick(TIMING.cycles_per_frame());
    }

    fn ac_rms(s: &mut LunarLanderDiscreteSound) -> f64 {
        let mut buf = vec![0i16; 16384];
        let n = s.fill_audio(&mut buf);
        if n == 0 {
            return 0.0;
        }
        let mean = buf[..n].iter().map(|&v| v as f64).sum::<f64>() / n as f64;
        let ac: f64 = buf[..n].iter().map(|&v| (v as f64 - mean).powi(2)).sum();
        (ac / n as f64).sqrt()
    }

    #[test]
    fn sound_register_maps_all_fields() {
        let mut s = LunarLanderDiscreteSound::new();
        s.write_sound_register(0b0011_1101); // thrust 5, explode, 3k, 6k
        assert_eq!(s.sound_reg & 0x07, 0x05);
        let regs = s.debug_registers();
        let get = |name: &str| regs.iter().find(|r| r.name == name).unwrap().value;
        assert_eq!(get("THRUST_DATA"), 5);
        assert_eq!(get("EXPLODE"), 1);
        assert_eq!(get("TONE_3K"), 1);
        assert_eq!(get("TONE_6K"), 1);
    }

    #[test]
    fn thrust_turns_off_when_data_clears() {
        // Regression: the resonant thrust band-pass must not latch "on". Drive
        // full thrust, then clear it, and confirm the output decays to silence.
        let mut s = LunarLanderDiscreteSound::new();
        let mut discard = vec![0i16; 16384];
        s.write_sound_register(0x07); // full thrust
        for _ in 0..30 {
            run_frame(&mut s);
        }
        let on = ac_rms(&mut s);
        assert!(on > 100.0, "thrust should be audible while on, rms={on:.0}");

        // Release, let the resonant filter ring down, then measure the steady
        // state ~0.5 s later.
        while s.fill_audio(&mut discard) > 0 {}
        s.write_sound_register(0x00);
        for _ in 0..30 {
            run_frame(&mut s);
        }
        while s.fill_audio(&mut discard) > 0 {} // discard the ring-down transient
        for _ in 0..30 {
            run_frame(&mut s);
        }
        let off = ac_rms(&mut s);
        assert!(
            off < 20.0,
            "thrust should be ~silent after release, rms={off:.0} (was {on:.0})"
        );
    }

    #[test]
    fn tone_only_is_deterministic_and_non_silent() {
        let mut a = LunarLanderDiscreteSound::new();
        let mut b = LunarLanderDiscreteSound::new();
        a.write_sound_register(0x10); // 3 kHz tone only
        b.write_sound_register(0x10);
        run_frame(&mut a);
        run_frame(&mut b);
        let mut ba = vec![0i16; 8192];
        let mut bb = vec![0i16; 8192];
        let na = a.fill_audio(&mut ba);
        let nb = b.fill_audio(&mut bb);
        assert!(na > 0 && na == nb);
        assert_eq!(ba[..na], bb[..nb], "tone output must be deterministic");
        assert!(ba[..na].iter().any(|&v| v != 0), "3 kHz tone non-silent");
    }

    #[test]
    fn thrust_and_explosion_are_audible() {
        // Thrust: full throttle, no explosion.
        let mut s = LunarLanderDiscreteSound::new();
        s.write_sound_register(0x07);
        for _ in 0..6 {
            run_frame(&mut s);
        }
        assert!(ac_rms(&mut s) > 150.0, "thrust should be audible");

        // The explosion's switch takes the throttle's node, so drive both.
        let mut s = LunarLanderDiscreteSound::new();
        s.write_sound_register(0x07 | 0x08);
        for _ in 0..6 {
            run_frame(&mut s);
        }
        assert!(ac_rms(&mut s) > 150.0, "explosion should be audible");
    }

    /// Settle a register value past the power-on transient and the band-pass
    /// ring-up, then return a second of output.
    fn settled(reg: u8) -> Vec<i16> {
        let mut s = LunarLanderDiscreteSound::new();
        s.write_sound_register(reg);
        for _ in 0..30 {
            run_frame(&mut s);
        }
        let mut discard = vec![0i16; 1 << 16];
        while s.fill_audio(&mut discard) > 0 {}
        let mut all = Vec::new();
        let mut buf = vec![0i16; 1 << 16];
        for _ in 0..60 {
            run_frame(&mut s);
            let n = s.fill_audio(&mut buf);
            all.extend_from_slice(&buf[..n]);
        }
        all
    }

    fn rms_of(samples: &[f64]) -> f64 {
        let mean = samples.iter().sum::<f64>() / samples.len() as f64;
        (samples.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
    }

    fn rms_i16(samples: &[i16]) -> f64 {
        rms_of(&samples.iter().map(|&v| v as f64).collect::<Vec<_>>())
    }

    /// The mixer's balance, as the drawing sets it.
    ///
    /// Two separate claims, because volume is two separate questions here.
    ///
    /// The RATIO of two voices is what the legs set, and an output-stage change
    /// cannot move it. Built from the parts, full thrust sits about 20 dB over
    /// a tone. The reference puts it at 24.4 and the device used to match that,
    /// but only through a make-up gain fitted to it; the reference's own gain
    /// table calls its thrust level a tweak. The two levels this rests on, the
    /// noise's 3.8 V and the gates' 4 V, are taken on trust, and a 4.6 dB
    /// difference is more than either could plausibly carry.
    ///
    /// The ABSOLUTE level is set by full scale being the op-amp's 5 V swing,
    /// doubled across the output pair. Against that, the tones land within
    /// 0.9 dB of the reference's calibration with nothing fitted, and full
    /// thrust near -27 dBFS.
    #[test]
    fn the_voices_keep_the_mixers_balance() {
        let thrust = rms_i16(&settled(0x07));
        let tone = rms_i16(&settled(0x10));

        let thrust_over_tone = 20.0 * (thrust / tone).log10();
        assert!(
            (18.3..21.3).contains(&thrust_over_tone),
            "thrust sits {thrust_over_tone:.1} dB over a tone; the drawing puts it at 19.8"
        );

        let thrust_dbfs = 20.0 * (thrust / 32767.0).log10();
        assert!(
            (-29.0..-25.0).contains(&thrust_dbfs),
            "full thrust measures {thrust_dbfs:.1} dBFS; the drawing puts it at -27.2"
        );
    }

    /// The explosion leg as drawn: R21 and C91 into a summing amp with C27
    /// across its feedback, which is a band-pass flat at C91/C27 from 159 Hz
    /// to 2258 Hz, taking the throttle's common node.
    ///
    /// Read that way it is well UNDER the thrust it always plays over, which
    /// is the finding recorded in `docs/schematics/llander-audio-output.md`.
    /// The noise sequence is deterministic and nothing clips, so the
    /// explosion's own contribution is exactly the sample-by-sample difference
    /// between full throttle with and without it. The reference puts the
    /// explosion about 15 dB OVER the thrust, by giving it the thrust leg's
    /// resistor at a flat gain, so this fails by a wide margin if that level
    /// comes back.
    #[test]
    fn the_drawn_explosion_sits_under_the_thrust() {
        let with = settled(0x0f);
        let without = settled(0x07);
        let explosion: Vec<f64> = with
            .iter()
            .zip(&without)
            .map(|(&a, &b)| a as f64 - b as f64)
            .collect();
        let thrust = rms_i16(&without);
        let db = 20.0 * (rms_of(&explosion) / thrust).log10();
        assert!(
            (-14.0..-9.0).contains(&db),
            "the explosion sits {db:.1} dB against the thrust; the drawing gives -11.1 \
             in the frequency domain and the device measured -11.75"
        );
    }

    /// The worst case the scenarios drive, full throttle with the explosion
    /// held, does not reach full scale. The board has the headroom (see
    /// `docs/schematics/llander-audio-output.md`), and the device used to clip
    /// 9.5 % of samples here only because its explosion level came from the
    /// reference's calibration.
    #[test]
    fn full_throttle_with_the_explosion_does_not_clip() {
        let peak = settled(0x0f)
            .iter()
            .map(|&v| (v as i32).abs())
            .max()
            .unwrap();
        assert!(peak < 32767 / 2, "peak {peak} reaches toward full scale");
    }

    /// The throttle's volume law, which is the board's three switched
    /// resistors and not a linear control.
    ///
    /// Each setting's corner and DC gain come from the solved network, and at
    /// the band-pass's 89.5 Hz center they put throttle 1 at 0.193 of full
    /// (-14.3 dB) and throttle 2 at 0.345 (-9.2 dB). A linear control gives
    /// -16.9 and -10.9. The windows sit between the two laws, so either
    /// setting fails if the throttle goes back to a linear multiply.
    ///
    /// The thrust voice is band-passed noise, so its RMS is read over a longer
    /// window than the balance test's; a second of it settles to well under
    /// the 1.3 dB that separates the laws at throttle 1.
    #[test]
    fn the_throttle_is_the_boards_switched_resistors_not_a_linear_control() {
        let level = |reg: u8| -> f64 {
            let mut s = LunarLanderDiscreteSound::new();
            s.write_sound_register(reg);
            for _ in 0..30 {
                run_frame(&mut s);
            }
            let mut discard = vec![0i16; 1 << 16];
            while s.fill_audio(&mut discard) > 0 {}
            let mut all = Vec::new();
            let mut buf = vec![0i16; 1 << 16];
            for _ in 0..60 {
                run_frame(&mut s);
                let n = s.fill_audio(&mut buf);
                all.extend_from_slice(&buf[..n]);
            }
            let mean = all.iter().map(|&v| v as f64).sum::<f64>() / all.len() as f64;
            let ac: f64 = all.iter().map(|&v| (v as f64 - mean).powi(2)).sum();
            (ac / all.len() as f64).sqrt()
        };
        let full = level(0x07);
        let db = |reg: u8| 20.0 * (level(reg) / full).log10();

        let one = db(0x01);
        assert!(
            (-15.6..-13.0).contains(&one),
            "throttle 1 sits {one:.2} dB under full; the board gives -14.3, a linear control -16.9"
        );
        let two = db(0x02);
        assert!(
            (-10.0..-8.4).contains(&two),
            "throttle 2 sits {two:.2} dB under full; the board gives -9.2, a linear control -10.9"
        );
    }

    #[test]
    fn noise_reset_pulses_without_panicking() {
        let mut s = LunarLanderDiscreteSound::new();
        s.write_sound_register(0x07);
        s.pulse_noise_reset();
        run_frame(&mut s);
        let mut buf = vec![0i16; 4096];
        assert!(s.fill_audio(&mut buf) > 0);
    }

    #[test]
    fn save_load_round_trip() {
        let mut s1 = LunarLanderDiscreteSound::new();
        s1.write_sound_register(0x07 | 0x10);
        run_frame(&mut s1);
        let mut discard = vec![0i16; 8192];
        while s1.fill_audio(&mut discard) > 0 {}

        let mut w = StateWriter::new();
        s1.save_state(&mut w);
        let data = w.into_vec();

        let mut s2 = LunarLanderDiscreteSound::new();
        let mut r = StateReader::new(&data);
        s2.load_state(&mut r).unwrap();

        assert_eq!(s2.sound_reg, s1.sound_reg);
        assert_eq!(s2.circuit.value(s2.ids.mix), s1.circuit.value(s1.ids.mix));

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
