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
//! # Known residual
//!
//! **The throttle's three resistors also set the noise filter's corner**, and
//! that is not modelled here: this builds one fixed 71 Hz corner (the value all
//! three switches closed give) and then multiplies by a linear volume. On the
//! board, closing only the 15k leg moves the corner to 10.6 Hz, so quieter
//! thrust is also darker thrust, and the volume law is compressed rather than
//! linear -- throttle 1 sits about 2.6 dB above where a linear DAC puts it.
//!
//! This cannot be found by comparison, which is why it is written down here. The
//! reference netlist has the identical gap, so both sides agree to 0.15
//! percentage points on every band at every throttle setting, and both are
//! wrong in the same way. Only the drawing says so.

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

/// Mixer leg levels, as PEAK-TO-PEAK swings, which is what a logic-gate output
/// on the far end of a resistor is.
///
/// Each is the leg's input swing times its resistor ratio into the summing amp.
/// The two noise legs go through R28 6.8k into R31 10k and appear on BOTH board
/// outputs in antiphase, so they carry a factor of two the tones do not: the
/// tones go through R29 and R30, 390k each, into R34 10k and appear on one
/// output only. That is where the 65-to-1 ratio between a tone and the thrust
/// comes from, and it is the drawing's, not a choice.
const LVL_TONE: f64 = 9.2;
const LVL_THRUST: f64 = 600.0;
const LVL_EXPLOSION: f64 = 1000.0;

// STILL FITTED, both of them, and both should come out of the drawing.
//
// THRUST_IN_GAIN sets the noise*throttle amplitude feeding the unfiltered
// explosion path; the thrust band-pass instead takes the normalized
// throttle*noise, and THRUST_OUT_GAIN supplies its post-filter make-up. The
// board has no counterpart for either: the explosion leg is R21 1.5k in series
// with C91 47nF, and the thrust leg is R28 6.8k, so their ratio is set by two
// resistors and a coupling capacitor rather than by these two numbers. The
// output normalization that used to sit beside them has been replaced by the
// mixer's own leg sum; these two have not. See phosphor-emulator-b72s.
const THRUST_IN_GAIN: f64 = 2400.0; // explosion noise * throttle amplitude
const THRUST_OUT_GAIN: f64 = 18.7; // post-band-pass make-up + trim
const OUTPUT_GAIN: f64 = 1.0 / LunarLanderDiscreteSound::MIX_FULL_SCALE;

fn build_circuit() -> (DiscreteCircuit, LunarLanderInputs) {
    let mut b = DiscreteCircuitBuilder::new(
        TIMING.cpu_clock_hz,
        phosphor_core::audio::host_sample_rate() as u64,
    );

    // --- Board-facing inputs ---
    let thrust_data = b.data_input("THRUST_DATA", 1.0); // normalized 0..1 (data/7)
    let tone3k_en = b.logic_input("TONE3K_EN");
    let tone6k_en = b.logic_input("TONE6K_EN");
    let explod_en = b.logic_input("EXPLOD_EN");
    let noise_reset = b.pulse_input("NOISE_RESET");

    // --- Shared noise -> RC low-pass (~71 Hz) ---
    let noise = b.custom(
        "NOISE",
        vec![noise_reset.into()],
        Box::new(LanderNoise {
            lfsr: LanderNoise::SEED,
            clock_acc: 0.0,
        }),
    );
    let noise_rc = b.rc_low_pass("NOISE_RC", noise, 2_247.0, 1e-6);

    // --- Thrust: noise scaled by the 3-bit throttle, through a resonant op-amp
    // multiple-feedback band-pass (R_in 1.17k / R_f 270k / C 0.1uF -> fc ~89.5 Hz,
    // Q ~7.6). The band-pass's component-set gain (center Rf/2·Rin ≈ 115) provides
    // the resonant make-up, so it takes the *normalized* throttle·noise — the
    // amplified `thrust_amp` below would slam the op-amp into its rails. ---
    let thrust_throttle = b.multiply("THRUST_THROTTLE", noise_rc, thrust_data);
    let thrust_bp = b.op_amp_band_pass(
        "THRUST_BP",
        thrust_throttle,
        &[1_170.0],
        270_000.0,
        0.1e-6,
        0.1e-6,
        0.0,
        -12.0,
        12.0,
    );
    let thrust_path = b.gain("THRUST_PATH", thrust_bp, THRUST_OUT_GAIN);

    // --- Explosion: the same noise*throttle, scaled up (unfiltered) and gated ---
    let thrust_in = b.gain("THRUST_IN", thrust_data, THRUST_IN_GAIN);
    let thrust_amp = b.multiply("THRUST_AMP", noise_rc, thrust_in);
    let explod_scaled = b.gain("EXPLOD_SCALED", thrust_amp, LVL_EXPLOSION / LVL_THRUST);
    let explod_gated = b.multiply("EXPLOD_GATE", explod_scaled, explod_en);

    // Sum thrust + explosion, then a shared output low-pass (560 Hz).
    let te_sum = b.add("TE_SUM", &[thrust_path, explod_gated]);
    let thrust_explod = b.low_pass_hz("THRUST_EXPLOD", te_sum, 560.0);

    // --- Alert tones (quiet relative to thrust/explosion) ---
    // The leg levels are PEAK-TO-PEAK swings, because that is what the gate
    // output on the other end of the 390k resistor is: a TTL high and a TTL low.
    // `fixed_square` swings +/-1, which is already 2 peak-to-peak, so the gain
    // that produces a 9.2 peak-to-peak leg is half the level and not the level.
    // Getting this wrong made both tones exactly 6 dB hot against the thrust,
    // which measured as a 6.4 dB balance error once the mixer normalization
    // above was corrected.
    let tone3k = b.fixed_square("TONE3K", 3_000.0);
    let tone3k_g = b.multiply("TONE3K_G", tone3k, tone3k_en);
    let tone3k_out = b.gain("TONE3K_OUT", tone3k_g, LVL_TONE / 2.0);

    let tone6k = b.fixed_square("TONE6K", 6_000.0);
    let tone6k_g = b.multiply("TONE6K_G", tone6k, tone6k_en);
    let tone6k_out = b.gain("TONE6K_OUT", tone6k_g, LVL_TONE / 2.0);

    // --- Final mix ---
    let mix = b.add("MIX", &[tone3k_out, tone6k_out, thrust_explod]);
    b.output(mix, OutputGain::linear(OUTPUT_GAIN));

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
    /// The `MIX` sum that renders as a full-scale output sample: the sum of the
    /// mixer's four leg levels, which is what "all four voices at once" comes
    /// to.
    ///
    /// Derived rather than chosen. The number that used to be here was 14347,
    /// with a comment saying it had been tuned against a capture, and it put
    /// the whole board 25 dB below the reference with every voice's SHAPE
    /// already correct. That is the signature of a normalization error rather
    /// than a modelling one, and 14347 had no counterpart anywhere on the
    /// drawing or in the mixer.
    ///
    /// Also exposed so a per-stage probe can be read at the same scale the
    /// mixer puts it at. A probe divided by anything else is measuring its own
    /// normalization rather than the voice's share of the mix.
    /// Halved because the leg levels are peak-to-peak, as the tone gain below
    /// says: their sum is the mix's whole swing, so full scale is half of it.
    /// The same convention, missed in both places, is why the first correction
    /// left every voice uniformly 6 dB short.
    pub const MIX_FULL_SCALE: f64 = (2.0 * LVL_TONE + LVL_THRUST + LVL_EXPLOSION) / 2.0;

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
            .set_data(self.ids.thrust_data, (data & 0x07) as f64 / 7.0);
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

        // Explosion reuses the thrust value as its volume (MAME), so drive both.
        let mut s = LunarLanderDiscreteSound::new();
        s.write_sound_register(0x07 | 0x08);
        for _ in 0..6 {
            run_frame(&mut s);
        }
        assert!(ac_rms(&mut s) > 150.0, "explosion should be audible");
    }

    /// The mixer's balance, which is the thing the leg levels decide and the
    /// thing that was wrong.
    ///
    /// Two separate claims, because volume is two separate questions here and
    /// the two defects this replaced were one of each.
    ///
    /// The RATIO of two voices is what the mixer legs set, and an output-stage
    /// change cannot move it. Note that it is NOT the ratio of the leg levels:
    /// a tone leg is 9.2 against the thrust's 600, but the thrust is
    /// band-passed noise whose RMS sits far below its leg level while a square
    /// wave's RMS is its level, so the measured ratio is about 24 dB and not
    /// the 36 dB the levels alone suggest. Both sides of the reference
    /// comparison agree on 24: ours 23.9, the netlist's 24.4.
    ///
    /// The ABSOLUTE level is what the output normalization sets, and the ratio
    /// is blind to it because it moves both voices together. So it is pinned
    /// separately and loosely.
    ///
    /// This exists because the two defects it would have caught both measured
    /// as plausible sound. The tones were 6 dB hot against the thrust from a
    /// peak-to-peak level driving a plus-or-minus-one square, and the whole
    /// board was a further 25 dB down from the same convention at the output
    /// stage. Neither is audible as "wrong" on its own.
    #[test]
    fn the_voices_keep_the_mixers_balance() {
        let level = |reg: u8| -> f64 {
            let mut s = LunarLanderDiscreteSound::new();
            s.write_sound_register(reg);
            // Settle past the power-on transient and the band-pass ring-up.
            for _ in 0..30 {
                run_frame(&mut s);
            }
            let mut discard = vec![0i16; 1 << 16];
            while s.fill_audio(&mut discard) > 0 {}
            for _ in 0..30 {
                run_frame(&mut s);
            }
            ac_rms(&mut s)
        };

        let thrust = level(0x07);
        let tone = level(0x10);
        let db = |a: f64, b: f64| 20.0 * (a / b).log10();

        // Full thrust against one alert tone. The window allows for the noise
        // voice's RMS not being settled to better than about half a decibel,
        // and is tight enough that the 6 dB tone error this replaced fails it.
        let thrust_over_tone = db(thrust, tone);
        assert!(
            (22.0..28.0).contains(&thrust_over_tone),
            "thrust sits {thrust_over_tone:.1} dB over a tone; the reference puts it at 24.4"
        );

        // And the absolute, which the ratio cannot see. Full thrust lands near
        // -21 dBFS; the wrong output normalization put it at -46.
        let thrust_dbfs = 20.0 * (thrust / 32767.0).log10();
        assert!(
            (-26.0..-16.0).contains(&thrust_dbfs),
            "full thrust measures {thrust_dbfs:.1} dBFS; the reference puts it at -21.0"
        );

        // The throttle is a linear multiply, so a third of it is a third of the
        // level. This is the check that would fail if the volume DAC were ever
        // rebuilt as the board's three switched resistors, which is NOT linear
        // and is the residual recorded against this device.
        let quarter = level(0x02);
        let ratio = db(thrust, quarter);
        let linear = 20.0 * (7.0f64 / 2.0).log10();
        assert!(
            (ratio - linear).abs() < 1.0,
            "throttle 7 over throttle 2 measured {ratio:.2} dB, a linear DAC gives {linear:.2}"
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
